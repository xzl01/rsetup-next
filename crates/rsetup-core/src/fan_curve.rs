use crate::{ActionRun, ActionStatus, HardwareError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

const CONFIG_FILE: &str = "/etc/rsetup-next/fan-curve.json";
const SERVICE_UNIT: &str = "rsetup-next-fan-curve.service";
const SERVICE_FILE: &str = "/usr/lib/systemd/system/rsetup-next-fan-curve.service";
const SYSTEMCTL: &str = "/usr/bin/systemctl";
const APPLY_LOCK_FILE: &str = "/run/lock/rsetup-next-fan-curve.lock";
const USER_SPACE_POLICY: &str = "user_space";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FanCurvePoint {
    pub temperature_c: f32,
    pub speed_percent: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FanCurveConfig {
    pub zone_id: String,
    pub cooling_device_id: String,
    pub poll_interval_ms: u32,
    pub hysteresis_c: f32,
    pub points: Vec<FanCurvePoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FanCurveRequest {
    pub enabled: bool,
    pub config: Option<FanCurveConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FanCurveZone {
    pub id: String,
    pub kind: String,
    pub temperature_c: Option<f32>,
    pub policy: Option<String>,
    pub supports_user_space: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FanCurveDevice {
    pub id: String,
    pub kind: String,
    pub current_state: Option<u32>,
    pub max_state: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanCurveStatus {
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub supported: bool,
    pub mutable: bool,
    pub active: bool,
    pub revision: String,
    pub config: Option<FanCurveConfig>,
    pub zones: Vec<FanCurveZone>,
    pub cooling_devices: Vec<FanCurveDevice>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FanCurveResolvedPoint {
    pub temperature_c: f32,
    pub speed_percent: u8,
    pub cooling_state: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanCurvePlan {
    pub synthetic: bool,
    pub revision: String,
    pub plan_token: String,
    pub request: FanCurveRequest,
    pub zone: Option<FanCurveZone>,
    pub cooling_device: Option<FanCurveDevice>,
    pub resolved_points: Vec<FanCurveResolvedPoint>,
    pub previous_policy: Option<String>,
    pub warnings: Vec<String>,
    pub requires_root: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanCurveApplyResult {
    pub run: ActionRun,
    pub plan: FanCurvePlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanCurveTick {
    pub at: DateTime<Utc>,
    pub temperature_c: Option<f32>,
    pub speed_percent: u8,
    pub cooling_state: u32,
    pub poll_interval_ms: u32,
    pub failsafe: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedFanCurve {
    config: FanCurveConfig,
    previous_policy: String,
}

struct FanCurveApplyLock {
    file: File,
}

impl FanCurveApplyLock {
    fn acquire(root: &Path) -> Result<Self, HardwareError> {
        Self::acquire_with_flags(root, libc::LOCK_EX)
    }

    fn acquire_with_flags(root: &Path, flags: libc::c_int) -> Result<Self, HardwareError> {
        let path = rooted_path(root, APPLY_LOCK_FILE);
        let parent = path
            .parent()
            .ok_or_else(|| HardwareError::Io("invalid fan curve lock path".into()))?;
        fs::create_dir_all(parent).map_err(|error| HardwareError::Io(error.to_string()))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .map_err(|error| {
                HardwareError::Io(format!("unable to open fan curve lock: {error}"))
            })?;
        loop {
            // SAFETY: flock only observes the valid descriptor owned by `file`.
            if unsafe { libc::flock(file.as_raw_fd(), flags) } == 0 {
                return Ok(Self { file });
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(HardwareError::Io(format!(
                "unable to lock fan curve updates: {error}"
            )));
        }
    }

    #[cfg(test)]
    fn try_acquire(root: &Path) -> Result<Self, HardwareError> {
        Self::acquire_with_flags(root, libc::LOCK_EX | libc::LOCK_NB)
    }
}

impl Drop for FanCurveApplyLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor remains valid until `file` is dropped after this method.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FanCurveManager {
    root: PathBuf,
    synthetic: bool,
}

impl FanCurveManager {
    pub(crate) fn new(synthetic: bool) -> Self {
        Self {
            root: PathBuf::from("/"),
            synthetic,
        }
    }

    #[cfg(test)]
    fn at_root(root: PathBuf) -> Self {
        Self {
            root,
            synthetic: false,
        }
    }

    pub(crate) fn status(&self) -> Result<FanCurveStatus, HardwareError> {
        if self.synthetic {
            return Ok(demo_status());
        }
        let zones = discover_zones(&self.root);
        let cooling_devices = discover_devices(&self.root);
        let saved = read_saved(&self.root)?;
        let supported =
            zones.iter().any(|zone| zone.supports_user_space) && !cooling_devices.is_empty();
        let writable = zones.iter().any(|zone| {
            zone.supports_user_space
                && writable_node(
                    &self
                        .root
                        .join("sys/class/thermal")
                        .join(&zone.id)
                        .join("policy"),
                )
        }) && cooling_devices.iter().any(|device| {
            writable_node(
                &self
                    .root
                    .join("sys/class/thermal")
                    .join(&device.id)
                    .join("cur_state"),
            )
        });
        let tools_available = rooted_path(&self.root, SYSTEMCTL).is_file()
            && rooted_path(&self.root, SERVICE_FILE).is_file();
        let mutable = supported && writable && tools_available;
        let unavailable_reason = if !zones.iter().any(|zone| zone.supports_user_space) {
            Some("No thermal zone with the user_space governor was detected.".into())
        } else if cooling_devices.is_empty() {
            Some("No controllable pwm-fan cooling device was detected.".into())
        } else if !writable {
            Some("The detected thermal and fan controls are read-only.".into())
        } else if !tools_available {
            Some("Install the rsetup-next fan curve service before enabling a curve.".into())
        } else {
            None
        };
        let config = saved.as_ref().map(|saved| saved.config.clone());
        let revision = status_revision(&zones, &cooling_devices, saved.as_ref());
        Ok(FanCurveStatus {
            collected_at: Utc::now(),
            synthetic: false,
            supported,
            mutable,
            active: saved.is_some() && service_active(&self.root),
            revision,
            config,
            zones,
            cooling_devices,
            unavailable_reason,
        })
    }

    pub(crate) fn plan(&self, request: &FanCurveRequest) -> Result<FanCurvePlan, HardwareError> {
        validate_request(request)?;
        let status = self.status()?;
        if !status.supported {
            return Err(HardwareError::Unsupported(
                status
                    .unavailable_reason
                    .unwrap_or_else(|| "fan curve control is unavailable".into()),
            ));
        }
        if !self.synthetic && !status.mutable {
            return Err(HardwareError::Unsupported(
                status
                    .unavailable_reason
                    .unwrap_or_else(|| "fan curve control is read-only".into()),
            ));
        }

        let (zone, cooling_device, resolved_points, previous_policy) = if request.enabled {
            let config = request.config.as_ref().expect("validated enabled request");
            let zone = status
                .zones
                .iter()
                .find(|zone| zone.id == config.zone_id)
                .cloned()
                .ok_or_else(|| {
                    HardwareError::InvalidInput(format!("unknown thermal zone {}", config.zone_id))
                })?;
            if !zone.supports_user_space {
                return Err(HardwareError::Conflict(format!(
                    "{} does not support the user_space thermal governor",
                    zone.id
                )));
            }
            let device = status
                .cooling_devices
                .iter()
                .find(|device| device.id == config.cooling_device_id)
                .cloned()
                .ok_or_else(|| {
                    HardwareError::InvalidInput(format!(
                        "unknown pwm-fan cooling device {}",
                        config.cooling_device_id
                    ))
                })?;
            if !self.synthetic
                && (!writable_node(
                    &self
                        .root
                        .join("sys/class/thermal")
                        .join(&zone.id)
                        .join("policy"),
                ) || !writable_node(
                    &self
                        .root
                        .join("sys/class/thermal")
                        .join(&device.id)
                        .join("cur_state"),
                ))
            {
                return Err(HardwareError::Unsupported(
                    "the selected thermal zone or pwm-fan control is read-only".into(),
                ));
            }
            validate_config(config)?;
            let resolved = config
                .points
                .iter()
                .map(|point| FanCurveResolvedPoint {
                    temperature_c: point.temperature_c,
                    speed_percent: point.speed_percent,
                    cooling_state: percent_to_state(point.speed_percent, device.max_state),
                })
                .collect();
            let saved = if self.synthetic {
                None
            } else {
                read_saved(&self.root)?
            };
            if !self.synthetic
                && saved
                    .as_ref()
                    .is_none_or(|saved| saved.config.zone_id != config.zone_id)
                && zone.policy.as_deref() == Some(USER_SPACE_POLICY)
            {
                return Err(HardwareError::Conflict(
                    "the selected zone is already controlled by user_space; restore a kernel governor or stop the external controller first".into(),
                ));
            }
            let previous_policy = if self.synthetic {
                Some("step_wise".into())
            } else {
                saved
                    .filter(|saved| saved.config.zone_id == config.zone_id)
                    .map(|saved| saved.previous_policy)
                    .or_else(|| zone.policy.clone())
            };
            (Some(zone), Some(device), resolved, previous_policy)
        } else {
            if status.config.is_none() {
                return Err(HardwareError::Conflict(
                    "no active fan curve configuration exists".into(),
                ));
            }
            let previous_policy = if self.synthetic {
                Some("step_wise".into())
            } else {
                read_saved(&self.root)?.map(|saved| saved.previous_policy)
            };
            (None, None, Vec::new(), previous_policy)
        };

        Ok(FanCurvePlan {
            synthetic: self.synthetic,
            revision: status.revision.clone(),
            plan_token: plan_token(&status.revision, request),
            request: request.clone(),
            zone,
            cooling_device,
            resolved_points,
            previous_policy,
            warnings: if request.enabled {
                vec![
                    "user_space_governor_replaces_kernel_fan_control".into(),
                    "invalid_curve_can_cause_overheating".into(),
                    "sensor_failure_forces_full_speed".into(),
                ]
            } else {
                vec!["previous_thermal_governor_will_be_restored".into()]
            },
            requires_root: true,
        })
    }

    pub(crate) fn apply_live(
        &self,
        request: &FanCurveRequest,
        supplied_token: &str,
    ) -> Result<FanCurveApplyResult, HardwareError> {
        if self.synthetic {
            return Err(HardwareError::RootRequired);
        }
        if supplied_token.trim().is_empty() {
            return Err(HardwareError::PlanRequired);
        }
        let _apply_lock = FanCurveApplyLock::acquire(&self.root)?;
        let plan = self.plan(request)?;
        if plan.plan_token != supplied_token {
            return Err(HardwareError::StalePlan);
        }
        let started_at = Utc::now();
        if request.enabled {
            self.enable_live(&plan)?;
        } else {
            self.disable_live()?;
        }
        let (action_id, title, summary) = if request.enabled {
            (
                "hardware.fan-curve.apply",
                "Apply fan curve",
                "Fan curve applied and the control service was started.",
            )
        } else {
            (
                "hardware.fan-curve.disable",
                "Disable fan curve",
                "Fan curve disabled and the previous thermal governor was restored.",
            )
        };
        Ok(FanCurveApplyResult {
            run: ActionRun {
                id: Uuid::new_v4().to_string(),
                action_id: action_id.into(),
                action_title: title.into(),
                status: ActionStatus::Succeeded,
                synthetic: false,
                summary: summary.into(),
                output: None,
                started_at,
                finished_at: Some(Utc::now()),
            },
            plan,
        })
    }

    pub(crate) fn tick(&self) -> Result<FanCurveTick, HardwareError> {
        if self.synthetic {
            let config = demo_config();
            let temperature_c = 54.8;
            let speed_percent = curve_percent(&config.points, temperature_c);
            return Ok(FanCurveTick {
                at: Utc::now(),
                temperature_c: Some(temperature_c),
                speed_percent,
                cooling_state: percent_to_state(speed_percent, 4),
                poll_interval_ms: config.poll_interval_ms,
                failsafe: false,
            });
        }
        let saved = read_saved(&self.root)?
            .ok_or_else(|| HardwareError::Unsupported("no saved fan curve".into()))?;
        validate_config(&saved.config)?;
        tick_saved(&self.root, &saved)
    }

    pub(crate) fn shutdown_failsafe(&self) -> Result<FanCurveTick, HardwareError> {
        if self.synthetic {
            return Err(HardwareError::RootRequired);
        }
        let saved = read_saved(&self.root)?
            .ok_or_else(|| HardwareError::Unsupported("no saved fan curve".into()))?;
        validate_config(&saved.config)?;
        let zone_path = self
            .root
            .join("sys/class/thermal")
            .join(&saved.config.zone_id);
        let device_path = self
            .root
            .join("sys/class/thermal")
            .join(&saved.config.cooling_device_id);
        let max_state = read_u32(device_path.join("max_state")).ok_or_else(|| {
            HardwareError::Unsupported(format!(
                "{} no longer exposes max_state",
                saved.config.cooling_device_id
            ))
        })?;
        if max_state == 0 {
            return Err(HardwareError::Unsupported(
                "pwm-fan cooling device has no controllable states".into(),
            ));
        }
        write_policy(&self.root, &saved.config.zone_id, USER_SPACE_POLICY)?;
        fs::write(device_path.join("cur_state"), format!("{max_state}\n"))
            .map_err(|error| HardwareError::Io(format!("unable to set pwm-fan state: {error}")))?;
        Ok(FanCurveTick {
            at: Utc::now(),
            temperature_c: read_temperature(zone_path.join("temp")),
            speed_percent: 100,
            cooling_state: max_state,
            poll_interval_ms: saved.config.poll_interval_ms,
            failsafe: true,
        })
    }

    fn enable_live(&self, plan: &FanCurvePlan) -> Result<(), HardwareError> {
        let config = plan.request.config.as_ref().expect("enabled plan");
        let zone = plan.zone.as_ref().expect("enabled plan zone");
        let device = plan.cooling_device.as_ref().expect("enabled plan device");
        let prior_file = fs::read(rooted_path(&self.root, CONFIG_FILE)).ok();
        let prior_saved = read_saved(&self.root)?;
        let prior_service_active = service_active(&self.root);
        let mut policies = BTreeMap::new();
        capture_policy(&self.root, &zone.id, &mut policies);
        if let Some(saved) = &prior_saved {
            capture_policy(&self.root, &saved.config.zone_id, &mut policies);
        }
        let mut states = BTreeMap::new();
        capture_state(&self.root, &device.id, &mut states);
        if let Some(saved) = &prior_saved {
            capture_state(&self.root, &saved.config.cooling_device_id, &mut states);
        }
        let operation: Result<(), HardwareError> = (|| {
            run_systemctl(&self.root, &["stop", SERVICE_UNIT])?;
            if let Some(saved) = &prior_saved
                && saved.config.zone_id != config.zone_id
            {
                write_policy(&self.root, &saved.config.zone_id, &saved.previous_policy)?;
            }
            write_policy(&self.root, &config.zone_id, USER_SPACE_POLICY)?;
            let previous_policy = prior_saved
                .as_ref()
                .filter(|saved| saved.config.zone_id == config.zone_id)
                .map(|saved| saved.previous_policy.clone())
                .or_else(|| plan.previous_policy.clone())
                .unwrap_or_else(|| USER_SPACE_POLICY.into());
            let saved = SavedFanCurve {
                config: config.clone(),
                previous_policy,
            };
            write_saved(&self.root, &saved)?;
            tick_saved(&self.root, &saved)?;
            run_systemctl(&self.root, &["enable", SERVICE_UNIT])?;
            run_systemctl(&self.root, &["restart", SERVICE_UNIT])?;
            Ok(())
        })();
        if let Err(error) = operation {
            let rollback = restore_snapshot(
                &self.root,
                prior_file.as_deref(),
                &policies,
                &states,
                prior_service_active,
            );
            return Err(match rollback {
                Ok(()) => HardwareError::Io(format!(
                    "fan curve update failed and the previous state was restored: {error}"
                )),
                Err(rollback) => HardwareError::Io(format!(
                    "fan curve update failed: {error}; rollback also failed: {rollback}"
                )),
            });
        }
        Ok(())
    }

    fn disable_live(&self) -> Result<(), HardwareError> {
        let saved = read_saved(&self.root)?
            .ok_or_else(|| HardwareError::Conflict("no saved fan curve".into()))?;
        let prior_file = fs::read(rooted_path(&self.root, CONFIG_FILE)).ok();
        let prior_service_active = service_active(&self.root);
        let mut policies = BTreeMap::new();
        capture_policy(&self.root, &saved.config.zone_id, &mut policies);
        let mut states = BTreeMap::new();
        capture_state(&self.root, &saved.config.cooling_device_id, &mut states);
        let operation: Result<(), HardwareError> = (|| {
            run_systemctl(&self.root, &["stop", SERVICE_UNIT])?;
            run_systemctl(&self.root, &["disable", SERVICE_UNIT])?;
            write_policy(&self.root, &saved.config.zone_id, &saved.previous_policy)?;
            match fs::remove_file(rooted_path(&self.root, CONFIG_FILE)) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(HardwareError::Io(error.to_string())),
            }
        })();
        if let Err(error) = operation {
            let rollback = restore_snapshot(
                &self.root,
                prior_file.as_deref(),
                &policies,
                &states,
                prior_service_active,
            );
            return Err(match rollback {
                Ok(()) => HardwareError::Io(format!(
                    "fan curve disable failed and the previous state was restored: {error}"
                )),
                Err(rollback) => HardwareError::Io(format!(
                    "fan curve disable failed: {error}; rollback also failed: {rollback}"
                )),
            });
        }
        Ok(())
    }
}

fn discover_zones(root: &Path) -> Vec<FanCurveZone> {
    let mut zones = Vec::new();
    let Ok(entries) = fs::read_dir(root.join("sys/class/thermal")) else {
        return zones;
    };
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        if !valid_thermal_id(&id, "thermal_zone") {
            continue;
        }
        let policies = read_trimmed(entry.path().join("available_policies"))
            .map(|value| {
                value
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        zones.push(FanCurveZone {
            id: id.clone(),
            kind: read_trimmed(entry.path().join("type")).unwrap_or(id),
            temperature_c: read_temperature(entry.path().join("temp")),
            policy: read_trimmed(entry.path().join("policy")),
            supports_user_space: policies.iter().any(|policy| policy == USER_SPACE_POLICY),
        });
    }
    zones.sort_by(|left, right| left.id.cmp(&right.id));
    zones
}

fn discover_devices(root: &Path) -> Vec<FanCurveDevice> {
    let mut devices = Vec::new();
    let Ok(entries) = fs::read_dir(root.join("sys/class/thermal")) else {
        return devices;
    };
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        if !valid_thermal_id(&id, "cooling_device") {
            continue;
        }
        let kind = read_trimmed(entry.path().join("type")).unwrap_or_else(|| id.clone());
        if !kind
            .to_ascii_lowercase()
            .replace('_', "-")
            .contains("pwm-fan")
        {
            continue;
        }
        let Some(max_state) = read_u32(entry.path().join("max_state")) else {
            continue;
        };
        if max_state == 0 {
            continue;
        }
        devices.push(FanCurveDevice {
            id,
            kind,
            current_state: read_u32(entry.path().join("cur_state")),
            max_state,
        });
    }
    devices.sort_by(|left, right| left.id.cmp(&right.id));
    devices
}

fn validate_request(request: &FanCurveRequest) -> Result<(), HardwareError> {
    match (request.enabled, request.config.as_ref()) {
        (true, Some(config)) => validate_config(config),
        (false, None) => Ok(()),
        (true, None) => Err(HardwareError::InvalidInput(
            "an enabled fan curve requires a configuration".into(),
        )),
        (false, Some(_)) => Err(HardwareError::InvalidInput(
            "a disabled fan curve must not include a configuration".into(),
        )),
    }
}

fn validate_config(config: &FanCurveConfig) -> Result<(), HardwareError> {
    if !valid_thermal_id(&config.zone_id, "thermal_zone")
        || !valid_thermal_id(&config.cooling_device_id, "cooling_device")
    {
        return Err(HardwareError::InvalidInput(
            "invalid fan curve target identifier".into(),
        ));
    }
    if !(500..=10_000).contains(&config.poll_interval_ms) {
        return Err(HardwareError::InvalidInput(
            "fan curve poll interval must be between 500 and 10000 ms".into(),
        ));
    }
    if !config.hysteresis_c.is_finite() || !(0.0..=10.0).contains(&config.hysteresis_c) {
        return Err(HardwareError::InvalidInput(
            "fan curve hysteresis must be between 0 and 10 C".into(),
        ));
    }
    if !(2..=8).contains(&config.points.len()) {
        return Err(HardwareError::InvalidInput(
            "fan curve requires between 2 and 8 points".into(),
        ));
    }
    let mut previous_temperature = None;
    let mut previous_speed = 0;
    for point in &config.points {
        if !point.temperature_c.is_finite()
            || !(0.0..=110.0).contains(&point.temperature_c)
            || point.speed_percent > 100
        {
            return Err(HardwareError::InvalidInput(
                "fan curve point is outside the supported range".into(),
            ));
        }
        if previous_temperature.is_some_and(|value| point.temperature_c <= value)
            || point.speed_percent < previous_speed
        {
            return Err(HardwareError::InvalidInput(
                "fan curve temperatures must rise and fan speed must not decrease".into(),
            ));
        }
        previous_temperature = Some(point.temperature_c);
        previous_speed = point.speed_percent;
    }
    let last = config.points.last().expect("validated point count");
    if last.speed_percent != 100 || last.temperature_c > 90.0 {
        return Err(HardwareError::InvalidInput(
            "fan curve must reach 100 percent at or below 90 C".into(),
        ));
    }
    Ok(())
}

fn curve_percent(points: &[FanCurvePoint], temperature_c: f32) -> u8 {
    if temperature_c <= points[0].temperature_c {
        return points[0].speed_percent;
    }
    for pair in points.windows(2) {
        let left = &pair[0];
        let right = &pair[1];
        if temperature_c <= right.temperature_c {
            let span = right.temperature_c - left.temperature_c;
            let progress = (temperature_c - left.temperature_c) / span;
            let speed = left.speed_percent as f32
                + (right.speed_percent as f32 - left.speed_percent as f32) * progress;
            return speed.round().clamp(0.0, 100.0) as u8;
        }
    }
    points.last().map_or(100, |point| point.speed_percent)
}

fn percent_to_state(percent: u8, max_state: u32) -> u32 {
    (u32::from(percent) * max_state).div_ceil(100)
}

fn tick_saved(root: &Path, saved: &SavedFanCurve) -> Result<FanCurveTick, HardwareError> {
    let config = &saved.config;
    let zone_path = root.join("sys/class/thermal").join(&config.zone_id);
    let device_path = root
        .join("sys/class/thermal")
        .join(&config.cooling_device_id);
    let max_state = read_u32(device_path.join("max_state")).ok_or_else(|| {
        HardwareError::Unsupported(format!(
            "{} no longer exposes max_state",
            config.cooling_device_id
        ))
    })?;
    if max_state == 0 {
        return Err(HardwareError::Unsupported(
            "pwm-fan cooling device has no controllable states".into(),
        ));
    }
    if read_trimmed(zone_path.join("policy")).as_deref() != Some(USER_SPACE_POLICY) {
        write_policy(root, &config.zone_id, USER_SPACE_POLICY)?;
    }
    let temperature_c = read_temperature(zone_path.join("temp"));
    let current_state = read_u32(device_path.join("cur_state")).unwrap_or(max_state);
    let (speed_percent, cooling_state, failsafe) = if let Some(temperature_c) = temperature_c {
        let speed_percent = curve_percent(&config.points, temperature_c);
        let mut cooling_state = percent_to_state(speed_percent, max_state);
        if cooling_state < current_state {
            let warmer_speed = curve_percent(&config.points, temperature_c + config.hysteresis_c);
            if percent_to_state(warmer_speed, max_state) >= current_state {
                cooling_state = current_state;
            }
        }
        (speed_percent, cooling_state, false)
    } else {
        (100, max_state, true)
    };
    fs::write(device_path.join("cur_state"), format!("{cooling_state}\n"))
        .map_err(|error| HardwareError::Io(format!("unable to set pwm-fan state: {error}")))?;
    Ok(FanCurveTick {
        at: Utc::now(),
        temperature_c,
        speed_percent,
        cooling_state,
        poll_interval_ms: config.poll_interval_ms,
        failsafe,
    })
}

fn read_saved(root: &Path) -> Result<Option<SavedFanCurve>, HardwareError> {
    let path = rooted_path(root, CONFIG_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(HardwareError::Io(error.to_string())),
    };
    let saved = serde_json::from_slice(&bytes)
        .map_err(|error| HardwareError::Io(format!("invalid saved fan curve: {error}")))?;
    Ok(Some(saved))
}

fn write_saved(root: &Path, saved: &SavedFanCurve) -> Result<(), HardwareError> {
    let path = rooted_path(root, CONFIG_FILE);
    let parent = path
        .parent()
        .ok_or_else(|| HardwareError::Io("invalid fan curve path".into()))?;
    fs::create_dir_all(parent).map_err(|error| HardwareError::Io(error.to_string()))?;
    let temporary = parent.join(format!(".fan-curve-{}.json", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| HardwareError::Io(error.to_string()))?;
    let bytes =
        serde_json::to_vec_pretty(saved).map_err(|error| HardwareError::Io(error.to_string()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| HardwareError::Io(error.to_string()))?;
    fs::rename(&temporary, &path).map_err(|error| HardwareError::Io(error.to_string()))
}

fn restore_snapshot(
    root: &Path,
    prior_file: Option<&[u8]>,
    policies: &BTreeMap<String, String>,
    states: &BTreeMap<String, u32>,
    restart_service: bool,
) -> Result<(), HardwareError> {
    let config_path = rooted_path(root, CONFIG_FILE);
    if let Some(bytes) = prior_file {
        let saved: SavedFanCurve = serde_json::from_slice(bytes)
            .map_err(|error| HardwareError::Io(format!("invalid rollback curve: {error}")))?;
        write_saved(root, &saved)?;
    } else {
        let _ = fs::remove_file(config_path);
    }
    for (zone, policy) in policies {
        write_policy(root, zone, policy)?;
    }
    for (device, state) in states {
        fs::write(
            root.join("sys/class/thermal")
                .join(device)
                .join("cur_state"),
            format!("{state}\n"),
        )
        .map_err(|error| HardwareError::Io(error.to_string()))?;
    }
    if restart_service {
        run_systemctl(root, &["enable", SERVICE_UNIT])?;
        run_systemctl(root, &["restart", SERVICE_UNIT])?;
    } else {
        let _ = run_systemctl(root, &["disable", SERVICE_UNIT]);
    }
    Ok(())
}

fn capture_policy(root: &Path, zone: &str, values: &mut BTreeMap<String, String>) {
    if values.contains_key(zone) {
        return;
    }
    if let Some(value) = read_trimmed(root.join("sys/class/thermal").join(zone).join("policy")) {
        values.insert(zone.into(), value);
    }
}

fn capture_state(root: &Path, device: &str, values: &mut BTreeMap<String, u32>) {
    if values.contains_key(device) {
        return;
    }
    if let Some(value) = read_u32(
        root.join("sys/class/thermal")
            .join(device)
            .join("cur_state"),
    ) {
        values.insert(device.into(), value);
    }
}

fn write_policy(root: &Path, zone: &str, policy: &str) -> Result<(), HardwareError> {
    if !valid_thermal_id(zone, "thermal_zone") || !valid_policy(policy) {
        return Err(HardwareError::InvalidInput(
            "invalid thermal policy target".into(),
        ));
    }
    fs::write(
        root.join("sys/class/thermal").join(zone).join("policy"),
        format!("{policy}\n"),
    )
    .map_err(|error| HardwareError::Io(format!("unable to set {zone} policy: {error}")))
}

fn run_systemctl(root: &Path, arguments: &[&str]) -> Result<String, HardwareError> {
    let output = Command::new(rooted_path(root, SYSTEMCTL))
        .args(arguments)
        .output()
        .map_err(|error| HardwareError::Io(format!("unable to run systemctl: {error}")))?;
    if !output.status.success() {
        return Err(HardwareError::Io(format!(
            "systemctl {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}

fn service_active(root: &Path) -> bool {
    if root != Path::new("/") || !rooted_path(root, SYSTEMCTL).is_file() {
        return false;
    }
    Command::new(SYSTEMCTL)
        .args(["is-active", "--quiet", SERVICE_UNIT])
        .status()
        .is_ok_and(|status| status.success())
}

fn status_revision(
    zones: &[FanCurveZone],
    devices: &[FanCurveDevice],
    saved: Option<&SavedFanCurve>,
) -> String {
    let mut hash = StableHash::new();
    for zone in zones {
        hash.update(zone.id.as_bytes());
        hash.update(zone.kind.as_bytes());
        hash.update(zone.policy.as_deref().unwrap_or("").as_bytes());
        hash.update(&[u8::from(zone.supports_user_space)]);
    }
    for device in devices {
        hash.update(device.id.as_bytes());
        hash.update(device.kind.as_bytes());
        hash.update(&device.max_state.to_le_bytes());
    }
    if let Some(saved) = saved
        && let Ok(bytes) = serde_json::to_vec(saved)
    {
        hash.update(&bytes);
    }
    format!("fan-{:016x}", hash.finish())
}

fn plan_token(revision: &str, request: &FanCurveRequest) -> String {
    let mut hash = StableHash::new();
    hash.update(revision.as_bytes());
    if let Ok(bytes) = serde_json::to_vec(request) {
        hash.update(&bytes);
    }
    format!("fan-plan-{:016x}", hash.finish())
}

fn valid_thermal_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn valid_policy(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn read_temperature(path: PathBuf) -> Option<f32> {
    read_trimmed(path)
        .and_then(|value| value.parse::<f32>().ok())
        .map(|value| value / 1000.0)
        .filter(|value| value.is_finite())
}

fn read_u32(path: PathBuf) -> Option<u32> {
    read_trimmed(path).and_then(|value| value.parse().ok())
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn writable_node(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o222 != 0)
}

fn rooted_path(root: &Path, absolute: &str) -> PathBuf {
    root.join(absolute.trim_start_matches('/'))
}

fn demo_config() -> FanCurveConfig {
    FanCurveConfig {
        zone_id: "thermal_zone0".into(),
        cooling_device_id: "cooling_device0".into(),
        poll_interval_ms: 2_000,
        hysteresis_c: 2.0,
        points: vec![
            FanCurvePoint {
                temperature_c: 40.0,
                speed_percent: 20,
            },
            FanCurvePoint {
                temperature_c: 55.0,
                speed_percent: 45,
            },
            FanCurvePoint {
                temperature_c: 70.0,
                speed_percent: 75,
            },
            FanCurvePoint {
                temperature_c: 82.0,
                speed_percent: 100,
            },
        ],
    }
}

fn demo_status() -> FanCurveStatus {
    let config = demo_config();
    let saved = SavedFanCurve {
        config: config.clone(),
        previous_policy: "step_wise".into(),
    };
    let zones = vec![
        FanCurveZone {
            id: "thermal_zone0".into(),
            kind: "soc-thermal".into(),
            temperature_c: Some(54.8),
            policy: Some(USER_SPACE_POLICY.into()),
            supports_user_space: true,
        },
        FanCurveZone {
            id: "thermal_zone1".into(),
            kind: "gpu-thermal".into(),
            temperature_c: Some(50.2),
            policy: Some("step_wise".into()),
            supports_user_space: true,
        },
    ];
    let cooling_devices = vec![FanCurveDevice {
        id: "cooling_device0".into(),
        kind: "pwm-fan".into(),
        current_state: Some(2),
        max_state: 4,
    }];
    FanCurveStatus {
        collected_at: Utc::now(),
        synthetic: true,
        supported: true,
        mutable: true,
        active: true,
        revision: status_revision(&zones, &cooling_devices, Some(&saved)),
        config: Some(config),
        zones,
        cooling_devices,
        unavailable_reason: None,
    }
}

struct StableHash(u64);

impl StableHash {
    fn new() -> Self {
        Self(0xcbf29ce484222325)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }

    fn finish(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root() -> PathBuf {
        std::env::temp_dir().join(format!("rsetup-fan-{}", Uuid::new_v4()))
    }

    fn write_fixture(path: &Path, value: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }

    fn add_fixture_hardware(root: &Path) {
        let zone = root.join("sys/class/thermal/thermal_zone0");
        write_fixture(&zone.join("type"), "soc-thermal\n");
        write_fixture(&zone.join("temp"), "60000\n");
        write_fixture(&zone.join("policy"), "user_space\n");
        write_fixture(
            &zone.join("available_policies"),
            "step_wise user_space power_allocator\n",
        );
        let device = root.join("sys/class/thermal/cooling_device0");
        write_fixture(&device.join("type"), "pwm-fan\n");
        write_fixture(&device.join("cur_state"), "2\n");
        write_fixture(&device.join("max_state"), "4\n");
    }

    fn add_fixture_service(root: &Path) {
        write_fixture(&root.join("usr/bin/systemctl"), "fixture\n");
        write_fixture(
            &root.join("usr/lib/systemd/system/rsetup-next-fan-curve.service"),
            "fixture\n",
        );
    }

    #[test]
    fn validates_monotonic_safe_curve() {
        assert!(validate_config(&demo_config()).is_ok());
        let mut decreasing = demo_config();
        decreasing.points[2].speed_percent = 10;
        assert!(validate_config(&decreasing).is_err());
        let mut unsafe_top = demo_config();
        unsafe_top.points.last_mut().unwrap().speed_percent = 90;
        assert!(validate_config(&unsafe_top).is_err());
    }

    #[test]
    fn interpolates_curve_and_rounds_up_to_a_cooling_state() {
        let config = demo_config();
        assert_eq!(curve_percent(&config.points, 40.0), 20);
        assert_eq!(curve_percent(&config.points, 62.5), 60);
        assert_eq!(curve_percent(&config.points, 90.0), 100);
        assert_eq!(percent_to_state(1, 4), 1);
        assert_eq!(percent_to_state(75, 4), 3);
    }

    #[test]
    fn discovers_only_userspace_zones_and_pwm_fans_for_planning() {
        let root = fixture_root();
        add_fixture_hardware(&root);
        let zones = discover_zones(&root);
        let devices = discover_devices(&root);
        assert_eq!(zones.len(), 1);
        assert!(zones[0].supports_user_space);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].max_state, 4);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn daemon_tick_uses_curve_and_fails_safe_on_sensor_error() {
        let root = fixture_root();
        add_fixture_hardware(&root);
        let saved = SavedFanCurve {
            config: demo_config(),
            previous_policy: "step_wise".into(),
        };
        write_saved(&root, &saved).unwrap();
        let manager = FanCurveManager::at_root(root.clone());
        let tick = manager.tick().unwrap();
        assert_eq!(tick.temperature_c, Some(60.0));
        assert_eq!(tick.cooling_state, 3);
        assert!(!tick.failsafe);
        fs::write(
            root.join("sys/class/thermal/thermal_zone0/temp"),
            "invalid\n",
        )
        .unwrap();
        let tick = manager.tick().unwrap();
        assert_eq!(tick.cooling_state, 4);
        assert!(tick.failsafe);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shutdown_failsafe_forces_maximum_cooling_for_systemd_stop() {
        let root = fixture_root();
        add_fixture_hardware(&root);
        let saved = SavedFanCurve {
            config: demo_config(),
            previous_policy: "step_wise".into(),
        };
        write_saved(&root, &saved).unwrap();
        fs::write(
            root.join("sys/class/thermal/thermal_zone0/policy"),
            "step_wise\n",
        )
        .unwrap();
        fs::write(
            root.join("sys/class/thermal/cooling_device0/cur_state"),
            "1\n",
        )
        .unwrap();
        let tick = FanCurveManager::at_root(root.clone())
            .shutdown_failsafe()
            .unwrap();
        assert!(tick.failsafe);
        assert_eq!(tick.cooling_state, 4);
        assert_eq!(
            read_trimmed(root.join("sys/class/thermal/thermal_zone0/policy")).as_deref(),
            Some(USER_SPACE_POLICY)
        );
        assert_eq!(
            read_u32(root.join("sys/class/thermal/cooling_device0/cur_state")),
            Some(4)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn status_revision_binds_previous_policy() {
        let zones = demo_status().zones;
        let devices = demo_status().cooling_devices;
        let first = SavedFanCurve {
            config: demo_config(),
            previous_policy: "step_wise".into(),
        };
        let mut second = first.clone();
        second.previous_policy = "power_allocator".into();
        assert_ne!(
            status_revision(&zones, &devices, Some(&first)),
            status_revision(&zones, &devices, Some(&second))
        );
    }

    #[test]
    fn live_updates_use_an_exclusive_process_lock() {
        let root = fixture_root();
        let first = FanCurveApplyLock::acquire(&root).unwrap();
        assert!(FanCurveApplyLock::try_acquire(&root).is_err());
        drop(first);
        assert!(FanCurveApplyLock::try_acquire(&root).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn first_enable_refuses_to_take_over_an_external_userspace_governor() {
        let root = fixture_root();
        add_fixture_hardware(&root);
        add_fixture_service(&root);
        let manager = FanCurveManager::at_root(root.clone());
        let request = FanCurveRequest {
            enabled: true,
            config: Some(demo_config()),
        };
        assert!(matches!(
            manager.plan(&request),
            Err(HardwareError::Conflict(_))
        ));
        fs::write(
            root.join("sys/class/thermal/thermal_zone0/policy"),
            "step_wise\n",
        )
        .unwrap();
        let plan = manager.plan(&request).unwrap();
        assert_eq!(plan.previous_policy.as_deref(), Some("step_wise"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn demo_plan_binds_complete_curve_and_revision() {
        let manager = FanCurveManager::new(true);
        let request = FanCurveRequest {
            enabled: true,
            config: Some(demo_config()),
        };
        let plan = manager.plan(&request).unwrap();
        assert_eq!(plan.resolved_points.len(), 4);
        assert_eq!(plan.previous_policy.as_deref(), Some("step_wise"));
        let mut changed = request.clone();
        changed.config.as_mut().unwrap().points[1].speed_percent = 50;
        assert_ne!(plan.plan_token, plan_token(&plan.revision, &changed));
    }
}
