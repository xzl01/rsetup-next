use crate::{
    ActionRun, ActionStatus,
    transaction::{ProcessLock, atomic_replace},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use thiserror::Error;
#[cfg(test)]
use uuid::Uuid;

mod gpio;
mod overlay;
mod shared;
mod video;

pub use self::gpio::{GpioChip, GpioConnector, GpioPin, GpioStatus};
pub use self::overlay::{
    OverlayApplyResult, OverlayBootChange, OverlayBootConfig, OverlayChange, OverlayEntry,
    OverlayPlan, OverlayStatus,
};
pub(crate) use self::overlay::{read_overlays, uefi_boot_mode};
pub(crate) use self::shared::{display_path, fingerprint, hardware_run};
// Temporary until the B6 wrap-up: unmigrated entry code keeps consuming these two bindings.
use self::shared::{read_trimmed, with_rollback_result};
pub use self::video::{VideoDevice, VideoFrame, VideoStatus};

const THERMAL_POLICY_FILE: &str = "/etc/rsetup-next/thermal-policy";
const THERMAL_POLICY_UNIT: &str = "rsetup-next-thermal-policy.service";
const LED_STATE_FILE: &str = "/etc/rsetup-next/led-state.json";
const LED_STATE_UNIT: &str = "rsetup-next-led-state.service";

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedStatus {
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub supported: bool,
    pub mutable: bool,
    pub leds: Vec<LedDevice>,
    pub rgb_groups: Vec<RgbLedGroup>,
    pub saved_state: LedSavedState,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LedDevice {
    pub id: String,
    pub current_trigger: Option<String>,
    pub available_triggers: Vec<String>,
    pub brightness: Option<u32>,
    pub max_brightness: Option<u32>,
    pub supports_pattern: bool,
    pub rgb_group: Option<String>,
    pub rgb_channel: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RgbLedGroup {
    pub id: String,
    pub red: String,
    pub green: String,
    pub blue: String,
    pub max_brightness: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RgbLedConfig {
    pub group_id: String,
    pub mode: String,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub brightness: u8,
    pub cycle_ms: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LedSavedState {
    #[serde(default)]
    pub triggers: BTreeMap<String, String>,
    #[serde(default)]
    pub rgb: BTreeMap<String, RgbLedConfig>,
}

#[derive(Debug, Clone)]
struct RgbLedSnapshot {
    id: String,
    path: PathBuf,
    trigger: String,
    pattern: Option<Vec<u8>>,
    brightness: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum HardwareError {
    #[error("hardware tool is unavailable: {0}")]
    Unsupported(String),
    #[error("invalid hardware selection: {0}")]
    InvalidInput(String),
    #[error("hardware resource conflict: {0}")]
    Conflict(String),
    #[error("confirmation is required before changing hardware configuration")]
    ConfirmationRequired,
    #[error("preview the hardware change before applying it")]
    PlanRequired,
    #[error("the hardware state changed after preview; create a fresh plan")]
    StalePlan,
    #[error("changing hardware configuration requires root privileges")]
    RootRequired,
    #[error("administrator authorization canceled")]
    AuthorizationCanceled,
    #[error("administrator authorization failed: {0}")]
    Authorization(String),
    #[error("unable to manage hardware: {0}")]
    Io(String),
}

#[derive(Debug, Clone)]
pub(crate) struct HardwareManager {
    root: PathBuf,
    synthetic: bool,
}

impl HardwareManager {
    pub(crate) fn new(synthetic: bool) -> Self {
        Self {
            root: PathBuf::from("/"),
            synthetic,
        }
    }

    #[cfg(test)]
    pub(crate) fn at_root(root: PathBuf) -> Self {
        Self {
            root,
            synthetic: false,
        }
    }

    pub(crate) fn led_status(&self) -> Result<LedStatus, HardwareError> {
        if self.synthetic {
            return Ok(demo_led_status());
        }
        let nodes = self.led_nodes();
        let mut leds = nodes
            .iter()
            .map(|(id, path)| read_led_device(id, path))
            .collect::<Vec<_>>();
        leds.sort_by(|left, right| left.id.cmp(&right.id));

        let by_id = leds
            .iter()
            .map(|led| (led.id.as_str(), led))
            .collect::<BTreeMap<_, _>>();
        let mut channels = BTreeMap::<String, BTreeMap<String, String>>::new();
        for led in &leds {
            if let Some((group, channel)) = split_rgb_channel(&led.id) {
                channels
                    .entry(group)
                    .or_default()
                    .insert(channel.into(), led.id.clone());
            }
        }
        let mut rgb_groups = channels
            .into_iter()
            .filter_map(|(id, channels)| {
                let red = channels.get("red")?.clone();
                let green = channels.get("green")?.clone();
                let blue = channels.get("blue")?.clone();
                let members = [
                    by_id.get(red.as_str())?,
                    by_id.get(green.as_str())?,
                    by_id.get(blue.as_str())?,
                ];
                members
                    .iter()
                    .all(|led| {
                        led.supports_pattern
                            && led
                                .available_triggers
                                .iter()
                                .any(|trigger| trigger == "pattern")
                    })
                    .then(|| RgbLedGroup {
                        id,
                        red,
                        green,
                        blue,
                        max_brightness: members
                            .iter()
                            .filter_map(|led| led.max_brightness)
                            .min()
                            .unwrap_or(255),
                    })
            })
            .collect::<Vec<_>>();
        rgb_groups.sort_by(|left, right| left.id.cmp(&right.id));
        let groups = rgb_groups
            .iter()
            .map(|group| group.id.as_str())
            .collect::<BTreeSet<_>>();
        for led in &mut leds {
            if let Some((group, channel)) = split_rgb_channel(&led.id)
                && groups.contains(group.as_str())
            {
                led.rgb_group = Some(group);
                led.rgb_channel = Some(channel.into());
            }
        }
        let supported = !leds.is_empty();
        let mutable = leds.iter().any(|led| !led.available_triggers.is_empty());
        Ok(LedStatus {
            collected_at: Utc::now(),
            synthetic: false,
            supported,
            mutable,
            leds,
            rgb_groups,
            saved_state: read_led_saved_state(&self.root)?,
            unavailable_reason: if !supported {
                Some("No Linux LED class device was detected.".into())
            } else if !mutable {
                Some("The detected LED devices do not expose writable triggers.".into())
            } else {
                None
            },
        })
    }

    pub(crate) fn apply_led_trigger_live(
        &self,
        led_id: &str,
        trigger: &str,
    ) -> Result<ActionRun, HardwareError> {
        let _lock = ProcessLock::acquire(&self.root, "rsetup-next-led.lock")
            .map_err(|error| HardwareError::Io(error.to_string()))?;
        validate_led_id(led_id)?;
        validate_led_trigger(trigger)?;
        let status = self.led_status()?;
        let led = status
            .leds
            .iter()
            .find(|led| led.id == led_id)
            .ok_or_else(|| HardwareError::InvalidInput(format!("unknown LED {led_id}")))?;
        if !led.available_triggers.iter().any(|item| item == trigger) {
            return Err(HardwareError::InvalidInput(format!(
                "trigger {trigger} is not available for {led_id}"
            )));
        }
        enable_led_persistence(&self.root)?;
        let path = self
            .led_nodes()
            .remove(led_id)
            .ok_or_else(|| HardwareError::Unsupported(format!("LED {led_id} disappeared")))?;
        let previous_trigger = led.current_trigger.clone().ok_or_else(|| {
            HardwareError::Unsupported(format!(
                "LED {led_id} does not expose its current trigger for safe rollback"
            ))
        })?;
        fs::write(path.join("trigger"), format!("{trigger}\n"))
            .map_err(|error| HardwareError::Io(format!("unable to update {led_id}: {error}")))?;
        let mut saved = status.saved_state;
        saved.triggers.insert(led_id.into(), trigger.into());
        if let Err(error) = write_led_saved_state(&self.root, &saved) {
            let rollback = fs::write(path.join("trigger"), format!("{previous_trigger}\n"))
                .map_err(|rollback| {
                    HardwareError::Io(format!("unable to roll back {led_id}: {rollback}"))
                });
            return Err(with_rollback_result(error, rollback));
        }
        Ok(hardware_run(
            "hardware.led-trigger",
            "Set LED trigger",
            ActionStatus::Succeeded,
            false,
            "LED trigger applied and saved for boot.",
            Some(format!("led={led_id}\ntrigger={trigger}")),
            Utc::now(),
        ))
    }

    pub(crate) fn apply_rgb_led_live(
        &self,
        config: &RgbLedConfig,
    ) -> Result<ActionRun, HardwareError> {
        let _lock = ProcessLock::acquire(&self.root, "rsetup-next-led.lock")
            .map_err(|error| HardwareError::Io(error.to_string()))?;
        let status = self.validate_rgb_led_config(config)?;
        let group = status
            .rgb_groups
            .iter()
            .find(|group| group.id == config.group_id)
            .expect("validated RGB group must remain in the captured status");
        enable_led_persistence(&self.root)?;
        let snapshot = self.apply_rgb_led_transactional(&status, group, config)?;
        let mut saved = status.saved_state;
        saved.rgb.insert(config.group_id.clone(), config.clone());
        if let Err(error) = write_led_saved_state(&self.root, &saved) {
            return Err(with_rollback_result(
                error,
                rollback_rgb_led_snapshot(&snapshot),
            ));
        }
        Ok(hardware_run(
            "hardware.rgb-led",
            "Set RGB LED pattern",
            ActionStatus::Succeeded,
            false,
            "RGB LED pattern applied and saved for boot.",
            Some(format!(
                "group={}\nmode={}\ncolor=#{:02x}{:02x}{:02x}\nbrightness={}\ncycle_ms={}",
                config.group_id,
                config.mode,
                config.red,
                config.green,
                config.blue,
                config.brightness,
                config.cycle_ms
            )),
            Utc::now(),
        ))
    }

    pub(crate) fn validate_rgb_led_config(
        &self,
        config: &RgbLedConfig,
    ) -> Result<LedStatus, HardwareError> {
        validate_rgb_config(config)?;
        let status = self.led_status()?;
        if !status
            .rgb_groups
            .iter()
            .any(|group| group.id == config.group_id)
        {
            return Err(HardwareError::InvalidInput(format!(
                "unknown RGB LED group {}",
                config.group_id
            )));
        }
        Ok(status)
    }

    pub(crate) fn restore_led_state_live(&self) -> Result<ActionRun, HardwareError> {
        let _lock = ProcessLock::acquire(&self.root, "rsetup-next-led.lock")
            .map_err(|error| HardwareError::Io(error.to_string()))?;
        let saved = read_led_saved_state(&self.root)?;
        let status = self.led_status()?;
        let nodes = self.led_nodes();
        let mut restored = 0usize;
        for (led_id, trigger) in &saved.triggers {
            let Some(led) = status.leds.iter().find(|led| led.id == *led_id) else {
                continue;
            };
            if !led.available_triggers.iter().any(|item| item == trigger) {
                continue;
            }
            if let Some(path) = nodes.get(led_id) {
                fs::write(path.join("trigger"), format!("{trigger}\n"))
                    .map_err(|error| HardwareError::Io(error.to_string()))?;
                restored += 1;
            }
        }
        for config in saved.rgb.values() {
            let Some(group) = status
                .rgb_groups
                .iter()
                .find(|group| group.id == config.group_id)
            else {
                continue;
            };
            self.apply_rgb_led_transactional(&status, group, config)?;
            restored += 1;
        }
        Ok(hardware_run(
            "hardware.led-restore",
            "Restore LED state",
            ActionStatus::Succeeded,
            false,
            "Saved LED state restored.",
            Some(format!("restored={restored}")),
            Utc::now(),
        ))
    }

    fn apply_rgb_led_transactional(
        &self,
        status: &LedStatus,
        group: &RgbLedGroup,
        config: &RgbLedConfig,
    ) -> Result<Vec<RgbLedSnapshot>, HardwareError> {
        validate_rgb_config(config)?;
        let nodes = self.led_nodes();
        let patterns = rgb_patterns(group.max_brightness, config);
        let snapshot = [&group.red, &group.green, &group.blue]
            .into_iter()
            .map(|id| {
                let path = nodes
                    .get(id)
                    .ok_or_else(|| HardwareError::Unsupported(format!("LED {id} disappeared")))?;
                let trigger = status
                    .leds
                    .iter()
                    .find(|led| led.id == *id)
                    .and_then(|led| led.current_trigger.clone())
                    .ok_or_else(|| {
                        HardwareError::Unsupported(format!(
                            "LED {id} does not expose its current trigger for safe rollback"
                        ))
                    })?;
                let pattern = if trigger == "pattern" {
                    Some(fs::read(path.join("pattern")).map_err(|error| {
                        HardwareError::Io(format!("unable to snapshot pattern on {id}: {error}"))
                    })?)
                } else {
                    None
                };
                let brightness = fs::read(path.join("brightness")).map_err(|error| {
                    HardwareError::Io(format!("unable to snapshot brightness on {id}: {error}"))
                })?;
                Ok(RgbLedSnapshot {
                    id: (*id).clone(),
                    path: path.clone(),
                    trigger,
                    pattern,
                    brightness,
                })
            })
            .collect::<Result<Vec<_>, HardwareError>>()?;
        for ((id, pattern), previous) in [
            (&group.red, &patterns[0]),
            (&group.green, &patterns[1]),
            (&group.blue, &patterns[2]),
        ]
        .into_iter()
        .zip(&snapshot)
        {
            let applied = fs::write(previous.path.join("trigger"), "pattern\n")
                .map_err(|error| {
                    HardwareError::Io(format!("unable to select pattern on {id}: {error}"))
                })
                .and_then(|()| {
                    fs::write(previous.path.join("pattern"), format!("{pattern}\n")).map_err(
                        |error| {
                            HardwareError::Io(format!("unable to update pattern on {id}: {error}"))
                        },
                    )
                });
            if let Err(error) = applied {
                return Err(with_rollback_result(
                    error,
                    rollback_rgb_led_snapshot(&snapshot),
                ));
            }
        }
        Ok(snapshot)
    }

    fn led_nodes(&self) -> BTreeMap<String, PathBuf> {
        let mut nodes = BTreeMap::new();
        collect_led_directory(&self.root.join("sys/class/leds"), &mut nodes);
        for driver in ["leds-gpio", "leds_pwm"] {
            let root = self.root.join("sys/bus/platform/drivers").join(driver);
            let Ok(devices) = fs::read_dir(root) else {
                continue;
            };
            for device in devices.flatten() {
                collect_led_directory(&device.path().join("leds"), &mut nodes);
            }
        }
        nodes
    }

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

fn collect_led_directory(directory: &Path, nodes: &mut BTreeMap<String, PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        if validate_led_id(&id).is_ok() && entry.path().join("trigger").exists() {
            nodes.entry(id).or_insert_with(|| entry.path());
        }
    }
}

fn read_led_device(id: &str, path: &Path) -> LedDevice {
    let trigger_text = read_trimmed(path.join("trigger")).unwrap_or_default();
    let mut available_triggers = Vec::new();
    let mut current_trigger = None;
    for raw in trigger_text.split_whitespace() {
        let selected = raw.starts_with('[') && raw.ends_with(']');
        let trigger = raw.trim_matches(['[', ']']).to_owned();
        if trigger.is_empty() {
            continue;
        }
        if selected {
            current_trigger = Some(trigger.clone());
        }
        if validate_led_trigger(&trigger).is_ok() {
            available_triggers.push(trigger);
        }
    }
    available_triggers.sort();
    available_triggers.dedup();
    LedDevice {
        id: id.into(),
        current_trigger,
        supports_pattern: available_triggers
            .iter()
            .any(|trigger| trigger == "pattern"),
        available_triggers,
        brightness: read_trimmed(path.join("brightness")).and_then(|value| value.parse().ok()),
        max_brightness: read_trimmed(path.join("max_brightness"))
            .and_then(|value| value.parse().ok()),
        rgb_group: None,
        rgb_channel: None,
    }
}

fn split_rgb_channel(id: &str) -> Option<(String, &'static str)> {
    for (suffix, channel) in [
        ("-red", "red"),
        ("-green", "green"),
        ("-blue", "blue"),
        (":red", "red"),
        (":green", "green"),
        (":blue", "blue"),
    ] {
        if let Some(group) = id.strip_suffix(suffix).filter(|group| !group.is_empty()) {
            return Some((group.into(), channel));
        }
    }
    None
}

fn validate_led_id(id: &str) -> Result<(), HardwareError> {
    if id.is_empty()
        || id.len() > 128
        || id.contains("..")
        || !id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-' | b':')
        })
    {
        return Err(HardwareError::InvalidInput("invalid LED identifier".into()));
    }
    Ok(())
}

fn validate_led_trigger(trigger: &str) -> Result<(), HardwareError> {
    if trigger.is_empty()
        || trigger.len() > 64
        || !trigger.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-' | b':')
        })
    {
        return Err(HardwareError::InvalidInput("invalid LED trigger".into()));
    }
    Ok(())
}

fn validate_rgb_config(config: &RgbLedConfig) -> Result<(), HardwareError> {
    validate_led_id(&config.group_id)?;
    if !matches!(config.mode.as_str(), "solid" | "breath" | "rainbow") {
        return Err(HardwareError::InvalidInput("invalid RGB LED mode".into()));
    }
    if config.brightness > 100 {
        return Err(HardwareError::InvalidInput(
            "RGB LED brightness must be between 0 and 100".into(),
        ));
    }
    if !(200..=60_000).contains(&config.cycle_ms) {
        return Err(HardwareError::InvalidInput(
            "RGB LED cycle must be between 200 and 60000 milliseconds".into(),
        ));
    }
    Ok(())
}

fn rgb_patterns(max_brightness: u32, config: &RgbLedConfig) -> [String; 3] {
    let scale = |channel: u8| {
        u32::from(channel) * u32::from(config.brightness) * max_brightness / (255 * 100)
    };
    match config.mode.as_str() {
        "breath" => {
            let half = (config.cycle_ms / 2).max(100);
            [scale(config.red), scale(config.green), scale(config.blue)]
                .map(|value| format!("0 {half} {value} {half}"))
        }
        "rainbow" => {
            let value = u32::from(config.brightness) * max_brightness / 100;
            let third = (config.cycle_ms / 3).max(100);
            [
                format!("{value} {third} 0 {third} 0 {third}"),
                format!("0 {third} {value} {third} 0 {third}"),
                format!("0 {third} 0 {third} {value} {third}"),
            ]
        }
        _ => {
            let hold = config.cycle_ms.max(200);
            [scale(config.red), scale(config.green), scale(config.blue)]
                .map(|value| format!("{value} {hold} {value} {hold}"))
        }
    }
}

fn rollback_rgb_led_snapshot(snapshot: &[RgbLedSnapshot]) -> Result<(), HardwareError> {
    let mut failures = Vec::new();
    for channel in snapshot {
        let restored = (|| -> std::io::Result<()> {
            // Restore brightness before the trigger: writing zero brightness after
            // selecting a trigger would deactivate it on Linux.
            fs::write(channel.path.join("trigger"), "none\n")?;
            fs::write(channel.path.join("brightness"), &channel.brightness)?;
            fs::write(
                channel.path.join("trigger"),
                format!("{}\n", channel.trigger),
            )?;
            if let Some(pattern) = &channel.pattern {
                fs::write(channel.path.join("pattern"), pattern)?;
            }
            Ok(())
        })();
        if let Err(error) = restored {
            failures.push(format!("{}: {error}", channel.id));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(HardwareError::Io(format!(
            "unable to roll back RGB LED channels: {}",
            failures.join(", ")
        )))
    }
}

fn read_led_saved_state(root: &Path) -> Result<LedSavedState, HardwareError> {
    let path = root.join(LED_STATE_FILE.trim_start_matches('/'));
    if !path.exists() {
        return Ok(LedSavedState::default());
    }
    let bytes = fs::read(path).map_err(|error| HardwareError::Io(error.to_string()))?;
    let state: LedSavedState = serde_json::from_slice(&bytes)
        .map_err(|error| HardwareError::Io(format!("invalid saved LED state: {error}")))?;
    for (led_id, trigger) in &state.triggers {
        validate_led_id(led_id)?;
        validate_led_trigger(trigger)?;
    }
    for (group_id, config) in &state.rgb {
        if group_id != &config.group_id {
            return Err(HardwareError::InvalidInput(
                "saved RGB LED key does not match its group".into(),
            ));
        }
        validate_rgb_config(config)?;
    }
    Ok(state)
}

fn write_led_saved_state(root: &Path, state: &LedSavedState) -> Result<(), HardwareError> {
    let path = root.join(LED_STATE_FILE.trim_start_matches('/'));
    let parent = path
        .parent()
        .ok_or_else(|| HardwareError::Io("invalid LED state path".into()))?;
    fs::create_dir_all(parent).map_err(|error| HardwareError::Io(error.to_string()))?;
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|error| HardwareError::Io(error.to_string()))?;
    atomic_replace(&path, &bytes).map_err(|error| HardwareError::Io(error.to_string()))
}

fn enable_led_persistence(root: &Path) -> Result<(), HardwareError> {
    if root != Path::new("/") {
        return Ok(());
    }
    let output = Command::new("systemctl")
        .args(["enable", LED_STATE_UNIT])
        .output()
        .map_err(|error| HardwareError::Io(format!("unable to enable LED persistence: {error}")))?;
    if !output.status.success() {
        return Err(HardwareError::Io(format!(
            "unable to enable LED persistence: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
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

fn demo_led_status() -> LedStatus {
    let triggers = vec![
        "activity".into(),
        "default-on".into(),
        "heartbeat".into(),
        "none".into(),
        "timer".into(),
    ];
    let mut leds = vec![
        LedDevice {
            id: "power".into(),
            current_trigger: Some("default-on".into()),
            available_triggers: triggers.clone(),
            brightness: Some(255),
            max_brightness: Some(255),
            supports_pattern: false,
            rgb_group: None,
            rgb_channel: None,
        },
        LedDevice {
            id: "status".into(),
            current_trigger: Some("heartbeat".into()),
            available_triggers: triggers,
            brightness: Some(96),
            max_brightness: Some(255),
            supports_pattern: false,
            rgb_group: None,
            rgb_channel: None,
        },
    ];
    for (channel, brightness) in [("red", 42), ("green", 104), ("blue", 220)] {
        leds.push(LedDevice {
            id: format!("rgb0-{channel}"),
            current_trigger: Some("pattern".into()),
            available_triggers: vec!["none".into(), "pattern".into(), "timer".into()],
            brightness: Some(brightness),
            max_brightness: Some(255),
            supports_pattern: true,
            rgb_group: Some("rgb0".into()),
            rgb_channel: Some(channel.into()),
        });
    }
    let rgb_config = RgbLedConfig {
        group_id: "rgb0".into(),
        mode: "breath".into(),
        red: 62,
        green: 132,
        blue: 255,
        brightness: 80,
        cycle_ms: 5_000,
    };
    LedStatus {
        collected_at: Utc::now(),
        synthetic: true,
        supported: true,
        mutable: true,
        leds,
        rgb_groups: vec![RgbLedGroup {
            id: "rgb0".into(),
            red: "rgb0-red".into(),
            green: "rgb0-green".into(),
            blue: "rgb0-blue".into(),
            max_brightness: 255,
        }],
        saved_state: LedSavedState {
            triggers: BTreeMap::from([
                ("power".into(), "default-on".into()),
                ("status".into(), "heartbeat".into()),
            ]),
            rgb: BTreeMap::from([("rgb0".into(), rgb_config)]),
        },
        unavailable_reason: None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn led_probe_and_native_writes_use_fixed_sysfs_nodes() {
        let root = std::env::temp_dir().join(format!("rsetup-led-{}", Uuid::new_v4()));
        for id in ["status", "rgb0-red", "rgb0-green", "rgb0-blue"] {
            let directory = root.join("sys/class/leds").join(id);
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join("trigger"),
                "none timer [heartbeat] pattern\n",
            )
            .unwrap();
            fs::write(directory.join("brightness"), "64\n").unwrap();
            fs::write(directory.join("max_brightness"), "255\n").unwrap();
        }
        let manager = HardwareManager::at_root(root.clone());
        let status = manager.led_status().unwrap();
        assert_eq!(status.leds.len(), 4);
        assert_eq!(status.rgb_groups.len(), 1);
        assert_eq!(status.leds[0].current_trigger.as_deref(), Some("heartbeat"));

        manager.apply_led_trigger_live("status", "timer").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("sys/class/leds/status/trigger")).unwrap(),
            "timer\n"
        );
        let saved = read_led_saved_state(&root).unwrap();
        assert_eq!(
            saved.triggers.get("status").map(String::as_str),
            Some("timer")
        );

        let config = RgbLedConfig {
            group_id: "rgb0".into(),
            mode: "breath".into(),
            red: 255,
            green: 64,
            blue: 0,
            brightness: 75,
            cycle_ms: 2_000,
        };
        manager.apply_rgb_led_live(&config).unwrap();
        assert!(
            fs::read_to_string(root.join("sys/class/leds/rgb0-red/pattern"))
                .unwrap()
                .contains("1000")
        );
        assert_eq!(
            read_led_saved_state(&root).unwrap().rgb.get("rgb0"),
            Some(&config)
        );
        // On reboot Linux only creates `pattern` after its trigger is selected.
        for id in ["rgb0-red", "rgb0-green", "rgb0-blue"] {
            let directory = root.join("sys/class/leds").join(id);
            fs::write(
                directory.join("trigger"),
                "[none] timer heartbeat pattern\n",
            )
            .unwrap();
            fs::remove_file(directory.join("pattern")).unwrap();
        }
        let restored = manager.restore_led_state_live().unwrap();
        assert_eq!(restored.output.as_deref(), Some("restored=2"));
        for id in ["rgb0-red", "rgb0-green", "rgb0-blue"] {
            let directory = root.join("sys/class/leds").join(id);
            assert_eq!(
                fs::read_to_string(directory.join("trigger")).unwrap(),
                "pattern\n"
            );
            assert!(directory.join("pattern").is_file());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn led_inputs_cannot_escape_sysfs_or_exceed_safe_ranges() {
        assert!(validate_led_id("../status").is_err());
        assert!(validate_led_id("status").is_ok());
        assert!(validate_led_trigger("timer;reboot").is_err());
        assert!(validate_led_trigger("default-on").is_ok());
        let invalid = RgbLedConfig {
            group_id: "rgb0".into(),
            mode: "strobe".into(),
            red: 255,
            green: 255,
            blue: 255,
            brightness: 100,
            cycle_ms: 5_000,
        };
        assert!(validate_rgb_config(&invalid).is_err());
    }

    #[test]
    fn led_changes_roll_back_when_persistence_fails() {
        let root = std::env::temp_dir().join(format!("rsetup-led-rollback-{}", Uuid::new_v4()));
        for id in ["status", "rgb0-red", "rgb0-green", "rgb0-blue"] {
            let directory = root.join("sys/class/leds").join(id);
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join("trigger"),
                "none timer [heartbeat] pattern\n",
            )
            .unwrap();
            fs::write(directory.join("brightness"), "64\n").unwrap();
            fs::write(directory.join("max_brightness"), "255\n").unwrap();
            if id.starts_with("rgb0-") {
                fs::write(
                    directory.join("trigger"),
                    "none timer heartbeat [pattern]\n",
                )
                .unwrap();
                fs::write(directory.join("pattern"), "0 100 0 100\n").unwrap();
            }
        }
        let state_directory = root.join("etc/rsetup-next");
        fs::create_dir_all(&state_directory).unwrap();
        // Readable prior state, but atomic replacement must reject a symlink.
        fs::write(state_directory.join("prior.json"), "{}\n").unwrap();
        std::os::unix::fs::symlink("prior.json", state_directory.join("led-state.json")).unwrap();
        let manager = HardwareManager::at_root(root.clone());

        assert!(manager.apply_led_trigger_live("status", "timer").is_err());
        assert_eq!(
            fs::read_to_string(root.join("sys/class/leds/status/trigger")).unwrap(),
            "heartbeat\n"
        );

        let config = RgbLedConfig {
            group_id: "rgb0".into(),
            mode: "rainbow".into(),
            red: 255,
            green: 255,
            blue: 255,
            brightness: 100,
            cycle_ms: 5_000,
        };
        assert!(manager.apply_rgb_led_live(&config).is_err());
        for id in ["rgb0-red", "rgb0-green", "rgb0-blue"] {
            assert_eq!(
                fs::read_to_string(root.join("sys/class/leds").join(id).join("trigger")).unwrap(),
                "pattern\n"
            );
            assert_eq!(
                fs::read_to_string(root.join("sys/class/leds").join(id).join("pattern")).unwrap(),
                "0 100 0 100\n"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rgb_rollback_restores_default_trigger_and_brightness_without_prior_pattern() {
        let root = std::env::temp_dir().join(format!("rsetup-led-default-{}", Uuid::new_v4()));
        for channel in ["red", "green", "blue"] {
            let directory = root.join(format!("sys/class/leds/rgb0-{channel}"));
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("trigger"), "[none] heartbeat pattern\n").unwrap();
            fs::write(directory.join("brightness"), "64\n").unwrap();
            fs::write(directory.join("max_brightness"), "255\n").unwrap();
        }
        let manager = HardwareManager::at_root(root.clone());
        let config = RgbLedConfig {
            group_id: "rgb0".into(),
            mode: "breath".into(),
            red: 255,
            green: 64,
            blue: 0,
            brightness: 75,
            cycle_ms: 2_000,
        };
        let status = manager.validate_rgb_led_config(&config).unwrap();
        let snapshot = manager
            .apply_rgb_led_transactional(&status, &status.rgb_groups[0], &config)
            .unwrap();
        for channel in &snapshot {
            assert!(channel.pattern.is_none());
            fs::write(channel.path.join("brightness"), "0\n").unwrap();
        }
        rollback_rgb_led_snapshot(&snapshot).unwrap();
        for channel in &snapshot {
            assert_eq!(
                fs::read_to_string(channel.path.join("brightness")).unwrap(),
                "64\n"
            );
            assert_eq!(
                fs::read_to_string(channel.path.join("trigger")).unwrap(),
                "none\n"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
