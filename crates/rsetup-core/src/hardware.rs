use crate::{ActionRun, ActionStatus};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use thiserror::Error;
use uuid::Uuid;

const THERMAL_POLICY_FILE: &str = "/etc/rsetup-next/thermal-policy";
const THERMAL_POLICY_UNIT: &str = "rsetup-next-thermal-policy.service";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayStatus {
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub supported: bool,
    pub mutable: bool,
    pub bootloader: String,
    pub directory: Option<String>,
    pub revision: String,
    pub overlays: Vec<OverlayEntry>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OverlayEntry {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub enabled: bool,
    pub exclusive: Vec<String>,
    pub packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OverlayChange {
    pub id: String,
    pub before_enabled: bool,
    pub after_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayPlan {
    pub synthetic: bool,
    pub revision: String,
    pub plan_token: String,
    pub selected_ids: Vec<String>,
    pub changes: Vec<OverlayChange>,
    pub warnings: Vec<String>,
    pub requires_root: bool,
    pub reboot_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayApplyResult {
    pub run: ActionRun,
    pub plan: OverlayPlan,
    pub reboot_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpioStatus {
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub supported: bool,
    pub serial_console_detected: bool,
    pub chips: Vec<GpioChip>,
    pub pins: Vec<GpioPin>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GpioChip {
    pub id: String,
    pub label: String,
    pub lines: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GpioPin {
    pub physical_pin: u8,
    pub label: String,
    pub kind: String,
    pub chip: Option<String>,
    pub offset: Option<u32>,
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoStatus {
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub supported: bool,
    pub capture_available: bool,
    pub devices: Vec<VideoDevice>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VideoDevice {
    pub id: String,
    pub path: String,
    pub name: String,
    pub driver: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoFrame {
    pub device_id: String,
    pub captured_at: DateTime<Utc>,
    pub mime_type: String,
    pub base64: String,
    pub synthetic: bool,
}

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
    #[error("preview the overlay change before applying it")]
    PlanRequired,
    #[error("the overlay directory changed after preview; create a fresh plan")]
    StalePlan,
    #[error("changing hardware configuration requires root privileges")]
    RootRequired,
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
    fn at_root(root: PathBuf) -> Self {
        Self {
            root,
            synthetic: false,
        }
    }

    pub(crate) fn overlay_status(&self) -> Result<OverlayStatus, HardwareError> {
        if self.synthetic {
            return Ok(demo_overlay_status());
        }
        let (bootloader, directory) = self.find_overlay_directory();
        let Some(directory) = directory else {
            return Ok(OverlayStatus {
                collected_at: Utc::now(),
                synthetic: false,
                supported: false,
                mutable: false,
                bootloader,
                directory: None,
                revision: overlay_revision(&[]),
                overlays: Vec::new(),
                unavailable_reason: Some("No managed overlay directory was detected.".into()),
            });
        };
        let overlays = read_overlays(&directory)?;
        let updater_available = command_exists("u-boot-update") && bootloader == "u-boot";
        let unavailable_reason = if overlays.is_empty() {
            Some("No device-tree overlay was found in the managed directory.".into())
        } else if !updater_available {
            Some("A supported U-Boot overlay updater was not detected.".into())
        } else {
            None
        };
        Ok(OverlayStatus {
            collected_at: Utc::now(),
            synthetic: false,
            supported: !overlays.is_empty(),
            mutable: !overlays.is_empty() && updater_available,
            bootloader,
            directory: Some(display_path(&self.root, &directory)),
            revision: overlay_revision(&overlays),
            overlays,
            unavailable_reason,
        })
    }

    pub(crate) fn plan_overlays(
        &self,
        selected_ids: &[String],
    ) -> Result<OverlayPlan, HardwareError> {
        let status = self.overlay_status()?;
        if !status.supported {
            return Err(HardwareError::Unsupported(
                status
                    .unavailable_reason
                    .unwrap_or_else(|| "overlay switching is not supported".into()),
            ));
        }
        if !self.synthetic && !status.mutable {
            return Err(HardwareError::Unsupported(
                status
                    .unavailable_reason
                    .unwrap_or_else(|| "overlay switching is read-only".into()),
            ));
        }
        let known = status
            .overlays
            .iter()
            .map(|overlay| (overlay.id.as_str(), overlay))
            .collect::<BTreeMap<_, _>>();
        let mut selected = selected_ids.to_vec();
        selected.sort();
        selected.dedup();
        for id in &selected {
            validate_overlay_id(id)?;
            if !known.contains_key(id.as_str()) {
                return Err(HardwareError::InvalidInput(format!("unknown overlay {id}")));
            }
        }
        let mut resource_owners = BTreeMap::<&str, &str>::new();
        for id in &selected {
            let overlay = known[id.as_str()];
            for resource in &overlay.exclusive {
                if let Some(owner) = resource_owners.insert(resource, &overlay.title) {
                    return Err(HardwareError::Conflict(format!(
                        "{} and {} both require {}",
                        owner, overlay.title, resource
                    )));
                }
            }
            for package in &overlay.packages {
                if !self.synthetic && !package_installed(package) {
                    return Err(HardwareError::Unsupported(format!(
                        "{} requires package {}",
                        overlay.title, package
                    )));
                }
            }
        }
        let selected_set = selected.iter().map(String::as_str).collect::<BTreeSet<_>>();
        let changes = status
            .overlays
            .iter()
            .filter_map(|overlay| {
                let after_enabled = selected_set.contains(overlay.id.as_str());
                (overlay.enabled != after_enabled).then(|| OverlayChange {
                    id: overlay.id.clone(),
                    before_enabled: overlay.enabled,
                    after_enabled,
                })
            })
            .collect::<Vec<_>>();
        let plan_token = overlay_plan_token(&status.revision, &selected, &changes);
        Ok(OverlayPlan {
            synthetic: self.synthetic,
            revision: status.revision,
            plan_token,
            selected_ids: selected,
            reboot_required: !changes.is_empty(),
            changes,
            warnings: vec!["applies_after_reboot".into(), "kernel_update_reset".into()],
            requires_root: true,
        })
    }

    pub(crate) fn apply_overlays_live(
        &self,
        selected_ids: &[String],
        plan_token: &str,
    ) -> Result<OverlayApplyResult, HardwareError> {
        let plan = self.plan_overlays(selected_ids)?;
        verify_overlay_plan(&plan, plan_token)?;
        let started_at = Utc::now();
        if plan.changes.is_empty() {
            return Ok(OverlayApplyResult {
                run: hardware_run(
                    "hardware.overlays",
                    "Switch device-tree overlays",
                    ActionStatus::Succeeded,
                    false,
                    "The selected overlays are already active.",
                    None,
                    started_at,
                ),
                reboot_required: false,
                plan,
            });
        }
        let (_, directory) = self.find_overlay_directory();
        let directory = directory.ok_or_else(|| {
            HardwareError::Unsupported("managed overlay directory disappeared".into())
        })?;
        let mut completed = Vec::<(PathBuf, PathBuf)>::new();
        for change in &plan.changes {
            let (from, to) = overlay_paths(&directory, &change.id, change.after_enabled);
            if let Err(error) = fs::rename(&from, &to) {
                rollback_renames(&completed);
                return Err(HardwareError::Io(format!(
                    "unable to update {}: {error}",
                    change.id
                )));
            }
            completed.push((from, to));
        }
        let update = Command::new("u-boot-update").output();
        match update {
            Ok(output) if output.status.success() => {
                let detail = bounded_output(&output.stdout, &output.stderr);
                Ok(OverlayApplyResult {
                    run: hardware_run(
                        "hardware.overlays",
                        "Switch device-tree overlays",
                        ActionStatus::Succeeded,
                        false,
                        "Overlay selection saved. Reboot to activate it.",
                        detail,
                        started_at,
                    ),
                    reboot_required: true,
                    plan,
                })
            }
            Ok(output) => {
                rollback_renames(&completed);
                let _ = Command::new("u-boot-update").output();
                Err(HardwareError::Io(format!(
                    "u-boot-update failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )))
            }
            Err(error) => {
                rollback_renames(&completed);
                let _ = Command::new("u-boot-update").output();
                Err(HardwareError::Io(format!(
                    "unable to start u-boot-update: {error}"
                )))
            }
        }
    }

    pub(crate) fn gpio_status(&self) -> Result<GpioStatus, HardwareError> {
        if self.synthetic {
            return Ok(demo_gpio_status());
        }
        let mut chips = Vec::new();
        let sys_gpio = self.root.join("sys/bus/gpio/devices");
        if let Ok(entries) = fs::read_dir(&sys_gpio) {
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().into_owned();
                if !id.starts_with("gpiochip") {
                    continue;
                }
                chips.push(GpioChip {
                    label: read_trimmed(entry.path().join("label")).unwrap_or_else(|| id.clone()),
                    lines: read_trimmed(entry.path().join("ngpio")).and_then(|v| v.parse().ok()),
                    id,
                });
            }
        }
        chips.sort_by(|left, right| left.id.cmp(&right.id));
        let supported = !chips.is_empty() || self.root.join("dev/gpiochip0").exists();
        let pins = (1..=40).map(|pin| self.probe_gpio_pin(pin)).collect();
        let cmdline = read_trimmed(self.root.join("proc/cmdline")).unwrap_or_default();
        let serial_console_detected = cmdline.split_whitespace().any(|item| {
            item.starts_with("console=ttyS")
                || item.starts_with("console=ttyAMA")
                || item.starts_with("console=ttyAML")
                || item.starts_with("console=ttyFIQ")
        });
        Ok(GpioStatus {
            collected_at: Utc::now(),
            synthetic: false,
            supported,
            serial_console_detected,
            chips,
            pins,
            unavailable_reason: (!supported)
                .then(|| "No GPIO character device was detected.".into()),
        })
    }

    fn probe_gpio_pin(&self, pin: u8) -> GpioPin {
        let kind = match pin {
            1 | 17 => "3v3",
            2 | 4 => "5v",
            6 | 9 | 14 | 20 | 25 | 30 | 34 | 39 => "ground",
            _ => "gpio",
        };
        if kind != "gpio" {
            return GpioPin {
                physical_pin: pin,
                label: kind.to_ascii_uppercase(),
                kind: kind.into(),
                chip: None,
                offset: None,
                value: None,
            };
        }
        let label = format!("PIN_{pin}");
        let location = command_text("gpiofind", &[&label]).and_then(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?.to_owned(), parts.next()?.parse::<u32>().ok()?))
        });
        let (chip, offset) = location.unzip();
        let value = chip.as_deref().zip(offset).and_then(|(chip, offset)| {
            let offset = offset.to_string();
            command_text("gpioget", &[chip, &offset])
                .or_else(|| command_text("gpioget", &["-c", chip, &offset]))
                .and_then(|value| match value.as_str() {
                    "0" | "inactive" => Some("low".into()),
                    "1" | "active" => Some("high".into()),
                    _ => None,
                })
        });
        GpioPin {
            physical_pin: pin,
            label,
            kind: if chip.is_some() { "gpio" } else { "unmapped" }.into(),
            chip,
            offset,
            value,
        }
    }

    pub(crate) fn video_status(&self) -> Result<VideoStatus, HardwareError> {
        if self.synthetic {
            return Ok(demo_video_status());
        }
        let base = self.root.join("sys/class/video4linux");
        let mut devices = Vec::new();
        if let Ok(entries) = fs::read_dir(base) {
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().into_owned();
                if !valid_video_id(&id) {
                    continue;
                }
                let path = format!("/dev/{id}");
                let driver = fs::read_link(entry.path().join("device/driver"))
                    .ok()
                    .and_then(|path| {
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    });
                devices.push(VideoDevice {
                    id: id.clone(),
                    path,
                    name: read_trimmed(entry.path().join("name")).unwrap_or(id),
                    driver,
                });
            }
        }
        devices.sort_by(|left, right| left.id.cmp(&right.id));
        let supported = !devices.is_empty();
        let capture_available = supported && command_exists("ffmpeg") && command_exists("timeout");
        Ok(VideoStatus {
            collected_at: Utc::now(),
            synthetic: false,
            supported,
            capture_available,
            devices,
            unavailable_reason: if !supported {
                Some("No Video4Linux device was detected.".into())
            } else if !capture_available {
                Some("Install ffmpeg to capture a webcam test frame.".into())
            } else {
                None
            },
        })
    }

    pub(crate) fn capture_video_frame(&self, device_id: &str) -> Result<VideoFrame, HardwareError> {
        if self.synthetic {
            if !matches!(device_id, "video0" | "video1") {
                return Err(HardwareError::InvalidInput(format!(
                    "unknown video device {device_id}"
                )));
            }
            return Ok(demo_video_frame(device_id));
        }
        if !valid_video_id(device_id) {
            return Err(HardwareError::InvalidInput(
                "invalid video device id".into(),
            ));
        }
        let status = self.video_status()?;
        if !status.devices.iter().any(|device| device.id == device_id) {
            return Err(HardwareError::InvalidInput(format!(
                "unknown video device {device_id}"
            )));
        }
        if !status.capture_available {
            return Err(HardwareError::Unsupported(
                status
                    .unavailable_reason
                    .unwrap_or_else(|| "video capture is unavailable".into()),
            ));
        }
        let device_path = format!("/dev/{device_id}");
        let output = Command::new("timeout")
            .args([
                "--signal=KILL",
                "8",
                "ffmpeg",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "video4linux2",
                "-i",
                &device_path,
                "-frames:v",
                "1",
                "-f",
                "image2pipe",
                "-vcodec",
                "mjpeg",
                "pipe:1",
            ])
            .output()
            .map_err(|error| HardwareError::Io(format!("unable to start capture: {error}")))?;
        if !output.status.success() {
            return Err(HardwareError::Io(format!(
                "camera capture failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        if output.stdout.is_empty() || output.stdout.len() > 8 * 1024 * 1024 {
            return Err(HardwareError::Io(
                "camera returned an empty or oversized frame".into(),
            ));
        }
        Ok(VideoFrame {
            device_id: device_id.into(),
            captured_at: Utc::now(),
            mime_type: "image/jpeg".into(),
            base64: encode_base64(&output.stdout),
            synthetic: false,
        })
    }

    pub(crate) fn thermal_status(&self) -> Result<ThermalStatus, HardwareError> {
        if self.synthetic {
            return Ok(demo_thermal_status());
        }
        let thermal_root = self.root.join("sys/class/thermal");
        let mut zones = Vec::new();
        let mut cooling_devices = Vec::new();
        if let Ok(entries) = fs::read_dir(&thermal_root) {
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().into_owned();
                if id.starts_with("thermal_zone") {
                    let available_policies = read_trimmed(entry.path().join("available_policies"))
                        .map(|value| value.split_whitespace().map(str::to_owned).collect())
                        .unwrap_or_default();
                    zones.push(ThermalZone {
                        id: id.clone(),
                        kind: read_trimmed(entry.path().join("type")).unwrap_or(id),
                        temperature_c: read_trimmed(entry.path().join("temp"))
                            .and_then(|value| value.parse::<f32>().ok())
                            .map(|value| value / 1000.0),
                        policy: read_trimmed(entry.path().join("policy")),
                        available_policies,
                    });
                } else if id.starts_with("cooling_device") {
                    cooling_devices.push(CoolingDevice {
                        id: id.clone(),
                        kind: read_trimmed(entry.path().join("type")).unwrap_or(id),
                        current_state: read_trimmed(entry.path().join("cur_state"))
                            .and_then(|value| value.parse().ok()),
                        max_state: read_trimmed(entry.path().join("max_state"))
                            .and_then(|value| value.parse().ok()),
                    });
                }
            }
        }
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
                restore_zone_policies(&self.root, &previous);
                return Err(HardwareError::Io(format!(
                    "unable to update {}: {error}",
                    zone.id
                )));
            }
        }
        if let Err(error) = write_thermal_policy(&self.root, policy) {
            restore_zone_policies(&self.root, &previous);
            return Err(error);
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

    fn find_overlay_directory(&self) -> (String, Option<PathBuf>) {
        let config = self.root.join("etc/default/u-boot");
        if let Some(value) = read_trimmed(&config).and_then(|content| {
            content.lines().find_map(|line| {
                let line = line.trim();
                let value = line.strip_prefix("U_BOOT_FDT_OVERLAYS_DIR=")?;
                Some(value.trim_matches(['\'', '"']).to_owned())
            })
        }) {
            let path = rooted_path(&self.root, &value);
            if path.is_dir() {
                return ("u-boot".into(), Some(path));
            }
        }
        for path in ["boot/dtbo", "boot/overlays"] {
            let candidate = self.root.join(path);
            if candidate.is_dir() {
                return ("u-boot".into(), Some(candidate));
            }
        }
        ("unknown".into(), None)
    }
}

fn read_overlays(directory: &Path) -> Result<Vec<OverlayEntry>, HardwareError> {
    let mut overlays = Vec::new();
    let entries = fs::read_dir(directory).map_err(|error| HardwareError::Io(error.to_string()))?;
    for entry in entries.flatten() {
        let filename = entry.file_name().to_string_lossy().into_owned();
        let (id, enabled) = if filename.ends_with(".dtbo.disabled") {
            (filename.trim_end_matches(".disabled").to_owned(), false)
        } else if filename.ends_with(".dtbo") {
            (filename, true)
        } else {
            continue;
        };
        validate_overlay_id(&id)?;
        let metadata = overlay_metadata(&entry.path());
        overlays.push(OverlayEntry {
            title: metadata
                .get("title")
                .and_then(|values| values.first())
                .cloned()
                .unwrap_or_else(|| humanize_overlay_id(&id)),
            description: metadata
                .get("description")
                .and_then(|values| values.first())
                .cloned(),
            category: metadata
                .get("category")
                .and_then(|values| values.first())
                .cloned(),
            exclusive: metadata.get("exclusive").cloned().unwrap_or_default(),
            packages: metadata.get("package").cloned().unwrap_or_default(),
            id,
            enabled,
        });
    }
    overlays.sort_by(|left, right| left.title.cmp(&right.title).then(left.id.cmp(&right.id)));
    Ok(overlays)
}

fn overlay_metadata(path: &Path) -> BTreeMap<String, Vec<String>> {
    let Ok(output) = Command::new("dtc")
        .args(["-I", "dtb", "-O", "dts"])
        .arg(path)
        .output()
    else {
        return BTreeMap::new();
    };
    if !output.status.success() {
        return BTreeMap::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(start) = text.find("metadata {") else {
        return BTreeMap::new();
    };
    let section = &text[start..];
    let end = section.find("\n\t};").unwrap_or(section.len());
    let section = &section[..end];
    let mut metadata = BTreeMap::new();
    for key in ["title", "description", "category", "exclusive", "package"] {
        if let Some(line) = section
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("{key} =")))
        {
            let values = quoted_values(line);
            if !values.is_empty() {
                metadata.insert(key.into(), values);
            }
        }
    }
    metadata
}

fn quoted_values(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut remainder = line;
    while let Some(start) = remainder.find('"') {
        remainder = &remainder[start + 1..];
        let Some(end) = remainder.find('"') else {
            break;
        };
        values.extend(
            remainder[..end]
                .split("\\0")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        );
        remainder = &remainder[end + 1..];
    }
    values
}

fn validate_overlay_id(id: &str) -> Result<(), HardwareError> {
    if id.len() > 128
        || !id.ends_with(".dtbo")
        || id.contains("..")
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        return Err(HardwareError::InvalidInput(format!(
            "invalid overlay id {id}"
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

fn valid_video_id(id: &str) -> bool {
    id.strip_prefix("video").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn verify_overlay_plan(plan: &OverlayPlan, supplied: &str) -> Result<(), HardwareError> {
    if supplied.trim().is_empty() {
        return Err(HardwareError::PlanRequired);
    }
    if supplied != plan.plan_token {
        return Err(HardwareError::StalePlan);
    }
    Ok(())
}

fn overlay_paths(directory: &Path, id: &str, after_enabled: bool) -> (PathBuf, PathBuf) {
    let enabled = directory.join(id);
    let disabled = directory.join(format!("{id}.disabled"));
    if after_enabled {
        (disabled, enabled)
    } else {
        (enabled, disabled)
    }
}

fn rollback_renames(completed: &[(PathBuf, PathBuf)]) {
    for (from, to) in completed.iter().rev() {
        let _ = fs::rename(to, from);
    }
}

fn restore_zone_policies(root: &Path, previous: &[(String, String)]) {
    for (zone, policy) in previous {
        let _ = fs::write(
            root.join("sys/class/thermal").join(zone).join("policy"),
            format!("{policy}\n"),
        );
    }
}

fn write_thermal_policy(root: &Path, policy: &str) -> Result<(), HardwareError> {
    let path = root.join(THERMAL_POLICY_FILE.trim_start_matches('/'));
    let parent = path
        .parent()
        .ok_or_else(|| HardwareError::Io("invalid thermal policy path".into()))?;
    fs::create_dir_all(parent).map_err(|error| HardwareError::Io(error.to_string()))?;
    let temporary = parent.join(format!(".thermal-policy.{}", std::process::id()));
    fs::write(&temporary, format!("{policy}\n"))
        .map_err(|error| HardwareError::Io(error.to_string()))?;
    fs::rename(&temporary, &path).map_err(|error| HardwareError::Io(error.to_string()))
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

fn overlay_revision(overlays: &[OverlayEntry]) -> String {
    let mut bytes = Vec::new();
    for overlay in overlays {
        bytes.extend_from_slice(overlay.id.as_bytes());
        bytes.push(u8::from(overlay.enabled));
    }
    fingerprint("overlays-v1", &bytes)
}

fn overlay_plan_token(revision: &str, selected: &[String], changes: &[OverlayChange]) -> String {
    let mut bytes = revision.as_bytes().to_vec();
    for id in selected {
        bytes.extend_from_slice(id.as_bytes());
        bytes.push(0);
    }
    for change in changes {
        bytes.extend_from_slice(change.id.as_bytes());
        bytes.push(u8::from(change.after_enabled));
    }
    fingerprint("overlay-plan-v1", &bytes)
}

fn fingerprint(prefix: &str, bytes: &[u8]) -> String {
    let mut left = 0xcbf2_9ce4_8422_2325u64;
    let mut right = 0x8422_2325_cbf2_9ce4u64;
    for byte in bytes {
        left ^= u64::from(*byte);
        left = left.wrapping_mul(0x100_0000_01b3);
        right ^= u64::from(*byte).rotate_left(1);
        right = right.wrapping_mul(0x100_0000_01b3).rotate_left(7);
    }
    format!("{prefix}-{left:016x}{right:016x}")
}

fn rooted_path(root: &Path, value: &str) -> PathBuf {
    root.join(value.trim_start_matches('/'))
}

fn display_path(root: &Path, path: &Path) -> String {
    if root == Path::new("/") {
        return path.display().to_string();
    }
    path.strip_prefix(root)
        .map(|path| format!("/{}", path.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
}

fn command_text(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(program).is_file())
    })
}

fn package_installed(package: &str) -> bool {
    Command::new("dpkg-query")
        .args(["-W", "-f=${db:Status-Abbrev}", package])
        .output()
        .is_ok_and(|output| {
            output.status.success() && String::from_utf8_lossy(&output.stdout).starts_with("ii ")
        })
}

fn bounded_output(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    let text = text.trim();
    (!text.is_empty()).then(|| text.chars().take(8_000).collect())
}

fn hardware_run(
    action_id: &str,
    title: &str,
    status: ActionStatus,
    synthetic: bool,
    summary: &str,
    output: Option<String>,
    started_at: DateTime<Utc>,
) -> ActionRun {
    ActionRun {
        id: Uuid::new_v4().to_string(),
        action_id: action_id.into(),
        action_title: title.into(),
        status,
        synthetic,
        summary: summary.into(),
        output,
        started_at,
        finished_at: Some(Utc::now()),
    }
}

fn humanize_overlay_id(id: &str) -> String {
    id.trim_end_matches(".dtbo")
        .replace(['-', '_'], " ")
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        output.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((value >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(value & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn demo_overlay_status() -> OverlayStatus {
    let overlays = vec![
        OverlayEntry {
            id: "rk3588-uart2-m0.dtbo".into(),
            title: "UART2 M0".into(),
            description: Some("Route UART2 to the 40-pin header.".into()),
            category: Some("Serial".into()),
            enabled: true,
            exclusive: vec!["uart2".into()],
            packages: vec![],
        },
        OverlayEntry {
            id: "rk3588-i2c3-m0.dtbo".into(),
            title: "I²C3 M0".into(),
            description: Some("Enable the I²C3 bus on header pins.".into()),
            category: Some("Bus".into()),
            enabled: true,
            exclusive: vec!["i2c3".into()],
            packages: vec![],
        },
        OverlayEntry {
            id: "rk3588-spi0-m2-cs0-spidev.dtbo".into(),
            title: "SPI0 M2".into(),
            description: Some("Expose SPI0 chip select 0 through spidev.".into()),
            category: Some("Bus".into()),
            enabled: false,
            exclusive: vec!["spi0".into()],
            packages: vec![],
        },
        OverlayEntry {
            id: "rk3588-can1-m0.dtbo".into(),
            title: "CAN1 M0".into(),
            description: Some("Enable CAN1 on the expansion header.".into()),
            category: Some("Field bus".into()),
            enabled: false,
            exclusive: vec!["can1".into()],
            packages: vec![],
        },
        OverlayEntry {
            id: "rk3588-pwm12-m0.dtbo".into(),
            title: "PWM12 M0".into(),
            description: Some("Expose PWM12 for fan or actuator control.".into()),
            category: Some("PWM".into()),
            enabled: false,
            exclusive: vec!["pwm12".into()],
            packages: vec![],
        },
        OverlayEntry {
            id: "rk3588-disable-led.dtbo".into(),
            title: "Disable status LED".into(),
            description: Some("Turn off the board status LED after boot.".into()),
            category: Some("Board".into()),
            enabled: false,
            exclusive: vec!["status-led".into()],
            packages: vec![],
        },
    ];
    OverlayStatus {
        collected_at: Utc::now(),
        synthetic: true,
        supported: true,
        mutable: true,
        bootloader: "u-boot".into(),
        directory: Some("/boot/dtbo".into()),
        revision: overlay_revision(&overlays),
        overlays,
        unavailable_reason: None,
    }
}

fn demo_gpio_status() -> GpioStatus {
    let pins = (1..=40)
        .map(|pin| {
            let kind = match pin {
                1 | 17 => "3v3",
                2 | 4 => "5v",
                6 | 9 | 14 | 20 | 25 | 30 | 34 | 39 => "ground",
                _ => "gpio",
            };
            GpioPin {
                physical_pin: pin,
                label: if kind == "gpio" {
                    format!("PIN_{pin}")
                } else {
                    kind.to_ascii_uppercase()
                },
                kind: kind.into(),
                chip: (kind == "gpio").then(|| format!("gpiochip{}", pin % 5)),
                offset: (kind == "gpio").then_some(u32::from(pin) + 32),
                value: (kind == "gpio").then(|| if pin % 3 == 0 { "high" } else { "low" }.into()),
            }
        })
        .collect();
    GpioStatus {
        collected_at: Utc::now(),
        synthetic: true,
        supported: true,
        serial_console_detected: true,
        chips: (0..5)
            .map(|index| GpioChip {
                id: format!("gpiochip{index}"),
                label: format!("RK3588 GPIO{index}"),
                lines: Some(32),
            })
            .collect(),
        pins,
        unavailable_reason: None,
    }
}

fn demo_video_status() -> VideoStatus {
    VideoStatus {
        collected_at: Utc::now(),
        synthetic: true,
        supported: true,
        capture_available: true,
        devices: vec![
            VideoDevice {
                id: "video0".into(),
                path: "/dev/video0".into(),
                name: "USB 1080p Camera".into(),
                driver: Some("uvcvideo".into()),
            },
            VideoDevice {
                id: "video1".into(),
                path: "/dev/video1".into(),
                name: "RKISP Main Path".into(),
                driver: Some("rkisp".into()),
            },
        ],
        unavailable_reason: None,
    }
}

fn demo_video_frame(device_id: &str) -> VideoFrame {
    let label = if device_id == "video0" {
        "USB 1080p Camera"
    } else {
        "RKISP Main Path"
    };
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="960" height="540" viewBox="0 0 960 540"><defs><linearGradient id="g" x1="0" x2="1" y1="0" y2="1"><stop stop-color="#dfe5f4"/><stop offset="1" stop-color="#f5ead7"/></linearGradient></defs><rect width="960" height="540" fill="url(#g)"/><circle cx="480" cy="240" r="120" fill="#fff" opacity=".72"/><path d="M390 210h120l72-45v150l-72-45H390z" fill="#5b68b5"/><text x="480" y="420" text-anchor="middle" font-family="sans-serif" font-size="28" fill="#293246">{label}</text><text x="480" y="458" text-anchor="middle" font-family="monospace" font-size="16" fill="#68748b">SYNTHETIC TEST FRAME</text></svg>"##
    );
    VideoFrame {
        device_id: device_id.into(),
        captured_at: Utc::now(),
        mime_type: "image/svg+xml".into(),
        base64: encode_base64(svg.as_bytes()),
        synthetic: true,
    }
}

fn demo_thermal_status() -> ThermalStatus {
    ThermalStatus {
        collected_at: Utc::now(),
        synthetic: true,
        supported: true,
        pwm_fan_detected: true,
        current_policy: Some("step_wise".into()),
        persisted_policy: Some("step_wise".into()),
        available_policies: vec![
            "bang_bang".into(),
            "power_allocator".into(),
            "step_wise".into(),
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
    fn demo_overlay_plan_is_exact_and_rejects_stale_token() {
        let manager = HardwareManager::new(true);
        let selected = vec!["rk3588-can1-m0.dtbo".to_owned()];
        let plan = manager.plan_overlays(&selected).unwrap();
        assert!(plan.reboot_required);
        assert!(verify_overlay_plan(&plan, "stale").is_err());
        assert!(verify_overlay_plan(&plan, &plan.plan_token).is_ok());
    }

    #[test]
    fn overlay_ids_cannot_escape_the_managed_directory() {
        assert!(validate_overlay_id("../evil.dtbo").is_err());
        assert!(validate_overlay_id("rk3588-uart.dtbo").is_ok());
    }

    #[test]
    fn live_overlay_probe_reads_enabled_and_disabled_files() {
        let root = std::env::temp_dir().join(format!("rsetup-hardware-{}", Uuid::new_v4()));
        let directory = root.join("boot/dtbo");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("uart.dtbo"), b"not-a-real-dtbo").unwrap();
        fs::write(directory.join("spi.dtbo.disabled"), b"not-a-real-dtbo").unwrap();
        let manager = HardwareManager::at_root(root.clone());
        let status = manager.overlay_status().unwrap();
        assert_eq!(status.overlays.len(), 2);
        assert!(
            status
                .overlays
                .iter()
                .any(|item| item.id == "uart.dtbo" && item.enabled)
        );
        assert!(
            status
                .overlays
                .iter()
                .any(|item| item.id == "spi.dtbo" && !item.enabled)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn base64_encoder_handles_padding() {
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
    }
}
