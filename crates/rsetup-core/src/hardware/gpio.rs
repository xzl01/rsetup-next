//! GPIO pin domain of the hardware module tree.
use super::{HardwareError, HardwareManager, read_trimmed};
use crate::hardware::OverlayStatus;
use crate::pinout::{
    FunctionEvidence, PinoutPin, PinoutProfile, profile_by_id, profile_for_root,
    resolve_function_evidence,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
#[cfg(test)]
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpioStatus {
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub supported: bool,
    pub serial_console_detected: bool,
    pub profile_id: Option<String>,
    pub board_name: Option<String>,
    pub profile_description: Option<String>,
    pub layout: String,
    pub connectors: Vec<GpioConnector>,
    pub configured_overlays: Vec<String>,
    #[serde(default)]
    pub configuration_known: bool,
    #[serde(default)]
    pub configuration_requires_authorization: bool,
    #[serde(default)]
    pub configuration_cached: bool,
    #[serde(default)]
    pub configuration_kernel: Option<String>,
    pub chips: Vec<GpioChip>,
    pub pins: Vec<GpioPin>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GpioConnector {
    pub id: String,
    pub label: String,
    pub pin_numbers: Vec<u8>,
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
    pub connector_id: String,
    pub label: String,
    pub kind: String,
    pub gpio_number: Option<String>,
    pub voltage: Option<String>,
    pub description: Option<String>,
    pub current_function: Option<String>,
    pub function_kind: String,
    pub function_source: String,
    pub source_detail: Option<String>,
    pub chip: Option<String>,
    pub offset: Option<u32>,
    pub direction: Option<String>,
    pub consumer: Option<String>,
}

impl HardwareManager {
    #[cfg(test)]
    pub(crate) fn gpio_status(&self) -> Result<GpioStatus, HardwareError> {
        self.gpio_status_for_profile(None)
    }

    #[cfg(test)]
    pub(crate) fn gpio_status_for_profile(
        &self,
        profile_override: Option<&str>,
    ) -> Result<GpioStatus, HardwareError> {
        self.gpio_status_with_overlays(profile_override, self.overlay_status().ok())
    }

    pub(crate) fn gpio_status_with_overlays(
        &self,
        profile_override: Option<&str>,
        overlay_status: Option<OverlayStatus>,
    ) -> Result<GpioStatus, HardwareError> {
        let mut chips = if self.synthetic {
            (0..5)
                .map(|index| GpioChip {
                    id: format!("gpiochip{index}"),
                    label: format!("GPIO controller {index}"),
                    lines: Some(32),
                })
                .collect::<Vec<_>>()
        } else {
            self.discover_gpio_chips()
        };
        chips.sort_by(|left, right| left.id.cmp(&right.id));
        let profile = if self.synthetic {
            match profile_override {
                Some("none") => None,
                Some(id) => profile_by_id(id),
                None => profile_by_id("rock5b"),
            }
        } else {
            profile_for_root(&self.root)
        };
        let supported = self.synthetic
            || profile.is_some()
            || !chips.is_empty()
            || self.root.join("dev/gpiochip0").exists();
        let configuration_requires_authorization = overlay_status
            .as_ref()
            .is_some_and(|status| status.requires_authorization);
        let configuration_cached = overlay_status.as_ref().is_some_and(|status| status.cached);
        let configuration_kernel = overlay_status
            .as_ref()
            .and_then(|status| status.boot_entry.as_ref().map(|entry| entry.kernel.clone()));
        let configuration_known = overlay_status
            .as_ref()
            .is_some_and(|status| status.configuration_known);
        let mut configured_overlays = overlay_status
            .filter(|status| status.configuration_known)
            .map(|status| {
                status
                    .overlays
                    .into_iter()
                    .filter(|overlay| overlay.enabled)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if self.synthetic
            && profile
                .as_ref()
                .is_none_or(|profile| profile.id != "rock5b")
        {
            configured_overlays.clear();
        }
        let overlay_evidence = configured_overlays
            .iter()
            .map(|overlay| FunctionEvidence {
                id: overlay.id.clone(),
                exclusive: overlay.exclusive.clone(),
                text: format!(
                    "{} {} {}",
                    overlay.id,
                    overlay.title,
                    overlay.description.as_deref().unwrap_or_default()
                ),
            })
            .collect::<Vec<_>>();
        let (profile_id, board_name, profile_description, layout, connectors, pins) =
            if let Some(profile) = profile {
                self.resolve_gpio_profile(profile, &overlay_evidence)
            } else {
                let connectors = vec![GpioConnector {
                    id: "main".into(),
                    label: "40-Pin GPIO Header".into(),
                    pin_numbers: (1..=40).collect(),
                }];
                let pins = (1..=40).map(|pin| self.probe_gpio_pin(pin)).collect();
                (None, None, None, "40-pin".into(), connectors, pins)
            };
        Ok(GpioStatus {
            collected_at: Utc::now(),
            synthetic: self.synthetic,
            supported,
            serial_console_detected: self.synthetic || self.serial_console_detected(),
            profile_id,
            board_name,
            profile_description,
            layout,
            connectors,
            configured_overlays: configured_overlays
                .into_iter()
                .map(|overlay| overlay.id)
                .collect(),
            configuration_known,
            configuration_requires_authorization,
            configuration_cached,
            configuration_kernel,
            chips,
            pins,
            unavailable_reason: (!supported)
                .then(|| "No GPIO character device was detected.".into()),
        })
    }

    fn discover_gpio_chips(&self) -> Vec<GpioChip> {
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
        chips
    }

    fn serial_console_detected(&self) -> bool {
        let cmdline = read_trimmed(self.root.join("proc/cmdline")).unwrap_or_default();
        cmdline.split_whitespace().any(|item| {
            // A serial console is `console=tty` followed by a driver letter
            // (`ttyS0`, `ttyAMA0`, `ttyAS0`, `ttyUSB0`); the virtual consoles are
            // `tty` followed by a digit (`tty0`, `tty1`). Listing SoC prefixes
            // instead missed Allwinner's `ttyAS`, so a board whose console is on
            // the header UART reported that it had none.
            item.strip_prefix("console=tty")
                .and_then(|rest| rest.chars().next())
                .is_some_and(|first| first.is_ascii_alphabetic())
        })
    }

    #[allow(clippy::type_complexity)]
    fn resolve_gpio_profile(
        &self,
        profile: PinoutProfile,
        overlay_evidence: &[FunctionEvidence],
    ) -> (
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        Vec<GpioConnector>,
        Vec<GpioPin>,
    ) {
        let connectors = profile
            .connectors
            .iter()
            .map(|connector| GpioConnector {
                id: connector.id.clone(),
                label: connector.name.clone(),
                pin_numbers: connector.pins.iter().map(|pin| pin.number).collect(),
            })
            .collect::<Vec<_>>();
        let pins = profile
            .connectors
            .iter()
            .flat_map(|connector| {
                connector
                    .pins
                    .iter()
                    .map(|pin| self.probe_profile_gpio_pin(&connector.id, pin, overlay_evidence))
            })
            .collect();
        (
            Some(profile.id),
            Some(profile.name),
            profile.description,
            profile.layout,
            connectors,
            pins,
        )
    }

    fn probe_profile_gpio_pin(
        &self,
        connector_id: &str,
        pin: &PinoutPin,
        overlay_evidence: &[FunctionEvidence],
    ) -> GpioPin {
        let physical_kind = gpio_physical_kind(pin);
        let resolved = match physical_kind.as_str() {
            "3v3" | "5v" => (
                pin.voltage.clone().or_else(|| Some(pin.name.clone())),
                "power".to_owned(),
                "fixed".to_owned(),
                None,
            ),
            "ground" => (Some("GND".into()), "ground".into(), "fixed".into(), None),
            "analog" => (
                Some(pin.name.clone()),
                "analog".into(),
                "fixed".into(),
                None,
            ),
            _ => {
                let function = resolve_function_evidence(pin, overlay_evidence);
                if function.kind == "conflict" {
                    (
                        None,
                        function.kind,
                        "conflict".into(),
                        function.source_detail,
                    )
                } else if function.name.is_some() {
                    (
                        function.name,
                        function.kind,
                        "overlay".into(),
                        function.source_detail,
                    )
                } else {
                    (
                        Some(pin.default_function.clone()),
                        if physical_kind == "special" {
                            "special"
                        } else {
                            "gpio"
                        }
                        .into(),
                        "default".into(),
                        None,
                    )
                }
            }
        };
        GpioPin {
            physical_pin: pin.number,
            connector_id: connector_id.into(),
            label: pin.name.clone(),
            kind: physical_kind,
            gpio_number: pin.gpio.clone(),
            voltage: pin.voltage.clone(),
            description: pin.description.clone(),
            current_function: resolved.0,
            function_kind: resolved.1,
            function_source: resolved.2,
            source_detail: resolved.3,
            chip: None,
            offset: None,
            direction: None,
            consumer: None,
        }
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
                connector_id: "main".into(),
                label: kind.to_ascii_uppercase(),
                kind: kind.into(),
                gpio_number: None,
                voltage: match kind {
                    "3v3" => Some("3.3V".into()),
                    "5v" => Some("5V".into()),
                    _ => None,
                },
                description: None,
                current_function: Some(match kind {
                    "ground" => "GND".into(),
                    "3v3" => "3.3V".into(),
                    "5v" => "5V".into(),
                    _ => kind.into(),
                }),
                function_kind: if kind == "ground" { "ground" } else { "power" }.into(),
                function_source: "fixed".into(),
                source_detail: None,
                chip: None,
                offset: None,
                direction: None,
                consumer: None,
            };
        }
        let label = format!("PIN_{pin}");
        GpioPin {
            physical_pin: pin,
            connector_id: "main".into(),
            label,
            kind: "gpio".into(),
            gpio_number: None,
            voltage: None,
            description: None,
            current_function: None,
            function_kind: "unassigned".into(),
            function_source: "unassigned".into(),
            source_detail: None,
            chip: None,
            offset: None,
            direction: None,
            consumer: None,
        }
    }
}

fn gpio_physical_kind(pin: &PinoutPin) -> String {
    match pin.kind.to_ascii_lowercase().as_str() {
        "power" if pin.voltage.as_deref() == Some("3.3V") => "3v3".into(),
        "power" if pin.voltage.as_deref() == Some("5V") => "5v".into(),
        "power" => "power".into(),
        "ground" => "ground".into(),
        "analog" => "analog".into(),
        "special" => "special".into(),
        _ => "gpio".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_gpio_combines_the_board_pinout_with_the_configured_overlays() {
        let status = HardwareManager::new(true).gpio_status().unwrap();
        assert_eq!(status.profile_id.as_deref(), Some("rock5b"));
        assert_eq!(status.board_name.as_deref(), Some("Radxa ROCK 5B"));
        let pin8 = status
            .pins
            .iter()
            .find(|pin| pin.physical_pin == 8)
            .unwrap();
        assert_eq!(pin8.current_function.as_deref(), Some("UART2_TX_M0"));
        assert_eq!(pin8.function_source, "overlay");
        let pin13 = status
            .pins
            .iter()
            .find(|pin| pin.physical_pin == 13)
            .unwrap();
        assert_eq!(pin13.current_function.as_deref(), Some("I2C3_SCL_M1"));
        assert_eq!(pin13.function_source, "overlay");
        let pin36 = status
            .pins
            .iter()
            .find(|pin| pin.physical_pin == 36)
            .unwrap();
        assert_eq!(pin36.current_function.as_deref(), Some("GPIO3_B1"));
        assert_eq!(pin36.function_source, "default");
    }

    #[test]
    fn demo_gpio_profile_override_keeps_the_debug_device_consistent() {
        let manager = HardwareManager::new(true);
        let dragon = manager.gpio_status_for_profile(Some("dragonQ6a")).unwrap();
        assert_eq!(dragon.profile_id.as_deref(), Some("dragonQ6a"));
        assert_eq!(dragon.board_name.as_deref(), Some("Dragon Q6A"));
        assert!(dragon.configured_overlays.is_empty());

        let q8b = manager.gpio_status_for_profile(Some("dragonQ8b")).unwrap();
        assert_eq!(q8b.board_name.as_deref(), Some("Radxa Dragon Q8B"));
        assert_eq!(q8b.pins.len(), 40);
        assert!(q8b.configured_overlays.is_empty());
        let pin16 = q8b.pins.iter().find(|pin| pin.physical_pin == 16).unwrap();
        assert_eq!(pin16.current_function.as_deref(), Some("GPIO_68"));
        assert_eq!(pin16.function_source, "default");
        assert!(pin16.chip.is_none());
        assert!(pin16.offset.is_none());

        let generic = manager.gpio_status_for_profile(Some("none")).unwrap();
        assert_eq!(generic.profile_id, None);
        assert_eq!(generic.pins.len(), 40);
    }

    #[test]
    fn live_q8b_gpio_uses_documented_defaults_and_detects_the_msm_console() {
        let root = std::env::temp_dir().join(format!("rsetup-q8b-pinout-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("proc/device-tree")).unwrap();
        fs::write(
            root.join("proc/device-tree/compatible"),
            b"radxa,dragon-q8b\0qcom,sc8280xp\0",
        )
        .unwrap();
        fs::write(
            root.join("proc/cmdline"),
            "root=UUID=example console=ttyMSM0,115200n8",
        )
        .unwrap();
        let status = HardwareManager::at_root(root.clone())
            .gpio_status()
            .unwrap();
        assert_eq!(status.profile_id.as_deref(), Some("dragonQ8b"));
        assert!(status.serial_console_detected);
        assert!(!status.synthetic);
        assert_eq!(status.pins.len(), 40);
        let pin8 = status
            .pins
            .iter()
            .find(|pin| pin.physical_pin == 8)
            .unwrap();
        assert_eq!(pin8.current_function.as_deref(), Some("GPIO_63"));
        assert_eq!(pin8.function_source, "default");
        assert!(
            status
                .pins
                .iter()
                .all(|pin| pin.chip.is_none() && pin.offset.is_none())
        );
        fs::write(root.join("proc/cmdline"), "root=UUID=example console=tty0").unwrap();
        assert!(!HardwareManager::at_root(root.clone()).serial_console_detected());
        fs::remove_dir_all(root).unwrap();
    }

    /// The flag drives a "the console owns these pins" warning, so it must not be a
    /// list of the SoC names someone happened to think of.
    #[test]
    fn a_serial_console_is_recognised_whatever_the_soc_calls_it() {
        let root = std::env::temp_dir().join(format!("rsetup-serial-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("proc")).expect("create proc");
        for (cmdline, expected) in [
            ("root=UUID=x console=ttyS0,115200n8", true),
            ("root=UUID=x console=ttyAMA0,115200", true),
            // The board that exposed this: Allwinner A733.
            ("root=UUID=x console=ttyAS0,115200n8", true),
            ("root=UUID=x console=ttyUSB0,115200", true),
            // Virtual consoles are not serial.
            ("root=UUID=x console=tty0", false),
            ("root=UUID=x console=tty1", false),
            ("root=UUID=x console=ttyS0,115200 console=tty1", true),
            ("root=UUID=x quiet", false),
        ] {
            fs::write(root.join("proc/cmdline"), cmdline).expect("write cmdline");
            assert_eq!(
                HardwareManager::at_root(root.clone()).serial_console_detected(),
                expected,
                "{cmdline}"
            );
        }
        fs::remove_dir_all(root).expect("remove scratch");
    }

    #[test]
    fn live_gpio_uses_the_configured_overlay_selection() {
        let root = std::env::temp_dir().join(format!("rsetup-pinmux-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("proc/device-tree")).unwrap();
        fs::write(root.join("proc/device-tree/model"), b"Radxa ROCK 5B\0").unwrap();
        fs::create_dir_all(root.join("boot/dtbo")).unwrap();
        fs::write(
            root.join("boot/dtbo/rk3588-i2c3-m1.dtbo"),
            b"configured-for-next-boot",
        )
        .unwrap();
        let status = HardwareManager::at_root(root.clone())
            .gpio_status()
            .unwrap();
        assert!(status.configuration_known);
        assert_eq!(
            status.configured_overlays,
            vec!["rk3588-i2c3-m1.dtbo".to_owned()]
        );
        let pin13 = status
            .pins
            .iter()
            .find(|pin| pin.physical_pin == 13)
            .unwrap();
        assert_eq!(pin13.current_function.as_deref(), Some("I2C3_SCL_M1"));
        assert_eq!(pin13.function_source, "overlay");
        let pin36 = status
            .pins
            .iter()
            .find(|pin| pin.physical_pin == 36)
            .unwrap();
        assert_eq!(pin36.current_function.as_deref(), Some("GPIO3_B1"));
        assert_eq!(pin36.function_source, "default");
        fs::remove_dir_all(root).unwrap();
    }
}
