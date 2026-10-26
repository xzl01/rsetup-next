//! Device-tree overlay domain of the hardware module tree (U-Boot track).
use super::{
    HardwareError, HardwareManager, display_path, fingerprint, hardware_run, read_trimmed,
    shared::command_exists, with_rollback_result,
};
use crate::{ActionRun, ActionStatus, transaction::ProcessLock};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};
#[cfg(test)]
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayStatus {
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub supported: bool,
    pub mutable: bool,
    pub bootloader: String,
    /// Whether overlay selection was read from a supported configuration backend.
    #[serde(default)]
    pub configuration_known: bool,
    #[serde(default)]
    pub requires_authorization: bool,
    #[serde(default)]
    pub cached: bool,
    #[serde(default)]
    pub boot_entry: Option<OverlayBootConfig>,
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
pub struct OverlayBootConfig {
    pub kernel: String,
    pub path: String,
    pub devicetree: Option<String>,
    pub available_devicetree: String,
    /// Ordered paths from the saved BLS entry, not inferred from file suffixes.
    pub overlays: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayBootChange {
    pub kernel: String,
    pub path: String,
    pub devicetree_before: Option<String>,
    pub devicetree_after: String,
    pub overlays_before: Vec<String>,
    pub overlays_after: Vec<String>,
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
    #[serde(default)]
    pub boot_change: Option<OverlayBootChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayApplyResult {
    pub run: ActionRun,
    pub plan: OverlayPlan,
    pub reboot_required: bool,
    #[serde(default)]
    pub status: Option<OverlayStatus>,
}

impl HardwareManager {
    pub(crate) fn overlay_status(&self) -> Result<OverlayStatus, HardwareError> {
        if self.synthetic {
            return Ok(demo_overlay_status());
        }
        // Firmware and hardware description are separate. Never fall back to
        // U-Boot files when the EFI backend is missing, ambiguous or unreadable.
        if let Some(bootloader) = uefi_boot_mode(&self.root) {
            return Ok(crate::efi_overlay::status(&self.root, bootloader));
        }
        let (bootloader, directory) = self.find_overlay_directory();
        let Some(directory) = directory else {
            return Ok(OverlayStatus {
                collected_at: Utc::now(),
                synthetic: false,
                supported: false,
                mutable: false,
                bootloader,
                configuration_known: false,
                requires_authorization: false,
                cached: false,
                boot_entry: None,
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
            configuration_known: true,
            requires_authorization: false,
            cached: false,
            boot_entry: None,
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
        self.plan_overlays_from_status(status, selected_ids)
    }

    pub(crate) fn plan_overlays_from_status(
        &self,
        status: OverlayStatus,
        selected_ids: &[String],
    ) -> Result<OverlayPlan, HardwareError> {
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
        let boot_change = crate::efi_overlay::boot_change(&status, &selected)?;
        Ok(OverlayPlan {
            synthetic: self.synthetic,
            revision: status.revision,
            plan_token,
            selected_ids: selected,
            reboot_required: !changes.is_empty(),
            changes,
            warnings: vec!["applies_after_reboot".into(), "kernel_update_reset".into()],
            requires_root: true,
            boot_change,
        })
    }

    pub(crate) fn apply_overlays_live(
        &self,
        selected_ids: &[String],
        plan_token: &str,
    ) -> Result<OverlayApplyResult, HardwareError> {
        if uefi_boot_mode(&self.root).is_some() {
            return crate::efi_overlay::apply(&self.root, selected_ids, plan_token, |status| {
                self.plan_overlays_from_status(status, selected_ids)
            });
        }
        let _lock = ProcessLock::acquire(&self.root, "rsetup-next-uboot-overlays.lock")
            .map_err(|error| HardwareError::Io(error.to_string()))?;
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
                status: None,
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
                return Err(with_rollback_result(
                    HardwareError::Io(format!("unable to update {}: {error}", change.id)),
                    rollback_renames(&completed),
                ));
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
                    status: None,
                    plan,
                })
            }
            Ok(output) => {
                let rollback = rollback_overlays(&completed);
                Err(with_rollback_result(
                    HardwareError::Io(format!(
                        "u-boot-update failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    )),
                    rollback,
                ))
            }
            Err(error) => {
                let rollback = rollback_overlays(&completed);
                Err(with_rollback_result(
                    HardwareError::Io(format!("unable to start u-boot-update: {error}")),
                    rollback,
                ))
            }
        }
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

pub(crate) fn uefi_boot_mode(root: &Path) -> Option<&'static str> {
    root.join("sys/firmware/efi").is_dir().then(|| {
        if root.join("sys/firmware/devicetree/base").is_dir()
            || root.join("proc/device-tree").is_dir()
        {
            "uefi-dt"
        } else {
            // Absence of DT is not by itself proof that ACPI is in use.
            "uefi"
        }
    })
}

pub(crate) fn read_overlays(directory: &Path) -> Result<Vec<OverlayEntry>, HardwareError> {
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

fn rollback_renames(completed: &[(PathBuf, PathBuf)]) -> Result<(), HardwareError> {
    let mut errors = Vec::new();
    for (from, to) in completed.iter().rev() {
        if let Err(error) = fs::rename(to, from) {
            errors.push(format!("{}: {error}", from.display()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(HardwareError::Io(errors.join("; ")))
    }
}

fn rollback_overlays(completed: &[(PathBuf, PathBuf)]) -> Result<(), HardwareError> {
    let renames = rollback_renames(completed);
    let update = Command::new("u-boot-update")
        .output()
        .map_err(|error| HardwareError::Io(error.to_string()))
        .and_then(|output| {
            if output.status.success() {
                Ok(())
            } else {
                Err(HardwareError::Io(format!(
                    "rollback u-boot-update failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )))
            }
        });
    match (renames, update) {
        (Err(first), Err(second)) => Err(HardwareError::Io(format!("{first}; {second}"))),
        (Err(error), _) | (_, Err(error)) => Err(error),
        _ => Ok(()),
    }
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

fn rooted_path(root: &Path, value: &str) -> PathBuf {
    root.join(value.trim_start_matches('/'))
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
            id: "rk3588-i2c3-m1.dtbo".into(),
            title: "I²C3 M1".into(),
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
        configuration_known: true,
        requires_authorization: false,
        cached: false,
        boot_entry: None,
        directory: Some("/boot/dtbo".into()),
        revision: overlay_revision(&overlays),
        overlays,
        unavailable_reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uefi_dt_never_uses_leftover_u_boot_overlays_or_writes_them() {
        let root = std::env::temp_dir().join(format!("rsetup-uefi-dt-{}", Uuid::new_v4()));
        for path in [
            "sys/firmware/efi",
            "sys/firmware/devicetree/base",
            "proc/device-tree",
            "boot/dtbo",
            "etc/default",
        ] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        fs::write(
            root.join("proc/device-tree/compatible"),
            b"radxa,dragon-q8b\0qcom,sc8280xp\0",
        )
        .unwrap();
        fs::write(
            root.join("etc/default/u-boot"),
            "U_BOOT_FDT_OVERLAYS_DIR='/boot/dtbo'\n",
        )
        .unwrap();
        let overlay = root.join("boot/dtbo/sc8280xp-uart18.dtbo");
        fs::write(&overlay, b"not-the-uefi-configuration").unwrap();
        let manager = HardwareManager::at_root(root.clone());
        let status = manager.overlay_status().unwrap();
        assert_eq!(status.bootloader, "uefi-dt");
        assert!(!status.supported && !status.mutable && !status.configuration_known);
        assert!(status.overlays.is_empty());
        assert!(status.unavailable_reason.is_some());
        assert!(manager.plan_overlays(&[]).is_err());
        assert!(manager.apply_overlays_live(&[], "invalid-token").is_err());
        assert_eq!(fs::read(&overlay).unwrap(), b"not-the-uefi-configuration");

        let gpio = manager.gpio_status().unwrap();
        assert_eq!(gpio.profile_id.as_deref(), Some("dragonQ8b"));
        assert_eq!(gpio.pins.len(), 40);
        assert!(!gpio.configuration_known);
        assert!(gpio.configured_overlays.is_empty());
        let pin16 = gpio.pins.iter().find(|pin| pin.physical_pin == 16).unwrap();
        assert_eq!(pin16.current_function.as_deref(), Some("GPIO_68"));
        assert_eq!(pin16.function_source, "default");
        assert_eq!(
            serde_json::to_value(gpio).unwrap()["configurationKnown"],
            false
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uefi_detection_keeps_firmware_and_dt_separate() {
        let root = std::env::temp_dir().join(format!("rsetup-uefi-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("boot/dtbo")).unwrap();
        fs::create_dir_all(root.join("sys/firmware/efi")).unwrap();
        assert_eq!(uefi_boot_mode(&root), Some("uefi"));
        assert!(
            !HardwareManager::at_root(root.clone())
                .overlay_status()
                .unwrap()
                .mutable
        );
        fs::create_dir_all(root.join("proc/device-tree")).unwrap();
        assert_eq!(uefi_boot_mode(&root), Some("uefi-dt"));
        fs::remove_dir(root.join("sys/firmware/efi")).unwrap();
        assert_eq!(uefi_boot_mode(&root), None);
        let status = HardwareManager::at_root(root.clone())
            .overlay_status()
            .unwrap();
        assert_eq!(status.bootloader, "u-boot");
        assert!(status.configuration_known);
        fs::remove_dir_all(root).unwrap();
    }

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
}
