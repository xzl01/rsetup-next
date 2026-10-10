use crate::{
    Alert, AlertLevel, Capability, DeviceIdentity, DeviceSnapshot, MetricSet, NetworkInterface,
    ProbeMode, ServiceState, ServiceSummary, StorageMetric, deviceinfo_probe::LocalProbe,
    hardware::HardwareManager,
};
use anyhow::Result;
use chrono::Utc;
use std::{env, fs, path::Path, process::Command};

pub fn collect_snapshot(requested_mode: ProbeMode) -> Result<DeviceSnapshot> {
    collect_with_probe(requested_mode, &mut LocalProbe::default())
}

pub(crate) fn collect_with_probe(
    requested_mode: ProbeMode,
    probe: &mut LocalProbe,
) -> Result<DeviceSnapshot> {
    let mode = resolve_mode(requested_mode);
    match mode {
        ProbeMode::Demo => {
            probe.invalidate_identity();
            Ok(demo_snapshot())
        }
        ProbeMode::Live | ProbeMode::Auto => live_snapshot(probe),
    }
}

fn resolve_mode(requested: ProbeMode) -> ProbeMode {
    match env::var("RSETUP_MODE").ok().as_deref() {
        Some("demo") => ProbeMode::Demo,
        Some("live") => ProbeMode::Live,
        _ if requested != ProbeMode::Auto => requested,
        _ if cfg!(target_os = "linux") => ProbeMode::Live,
        _ => ProbeMode::Demo,
    }
}

fn live_snapshot(probe: &mut LocalProbe) -> Result<DeviceSnapshot> {
    let observation = probe.collect();
    let temperature_c = observation.metrics.temperature_c;
    let storage = observation.storage;
    let interfaces = probe_interfaces();
    let services = vec![
        probe_service("ssh.service", "Remote shell"),
        probe_service("NetworkManager.service", "Network manager"),
        probe_service("docker.service", "Container runtime"),
    ];
    let hardware = HardwareManager::new(false);
    let overlays = hardware.overlay_status().ok();
    let video = hardware.video_status().ok();
    let capabilities = vec![
        inspected_capability(
            "device-tree",
            "Device-tree overlays",
            overlays.as_ref().is_some_and(|status| status.supported),
            "Overlay storage detected",
            overlays
                .as_ref()
                .and_then(|status| status.unavailable_reason.as_deref()),
        ),
        capability(
            "gpio",
            "GPIO",
            Path::new("/proc/device-tree/model").exists()
                || Path::new("/sys/firmware/devicetree/base/model").exists()
                || Path::new("/dev/gpiochip0").exists(),
            if overlays
                .as_ref()
                .is_some_and(|status| status.configuration_known)
            {
                "Overlay-aware 40-pin map"
            } else {
                "40-pin defaults · overlay configuration unread"
            },
        ),
        inspected_capability(
            "video",
            "Video capture",
            video.as_ref().is_some_and(|status| status.supported),
            "Video4Linux capture device",
            video
                .as_ref()
                .and_then(|status| status.unavailable_reason.as_deref()),
        ),
        capability(
            "thermal",
            "Thermal controls",
            Path::new("/sys/class/thermal").exists(),
            "Kernel thermal subsystem",
        ),
        capability(
            "led",
            "LED control",
            Path::new("/sys/class/leds").exists()
                || Path::new("/sys/bus/platform/drivers/leds-gpio").exists()
                || Path::new("/sys/bus/platform/drivers/leds_pwm").exists(),
            "Linux LED class devices",
        ),
        capability(
            "spi-flash",
            "SPI boot flash",
            spi_nor_detected(),
            "SPI NOR MTD device",
        ),
        storage_capability(
            observation.storage_counts.0,
            observation.storage_counts.1,
            observation.storage_counts.2,
        ),
    ];
    let mut alerts = Vec::new();
    if temperature_c.is_some_and(|value| value >= 80.0) {
        alerts.push(Alert {
            id: "thermal-high".into(),
            level: AlertLevel::Critical,
            title: "Thermal ceiling approaching".into(),
            detail: "Sustained operation above 80°C may throttle the board.".into(),
        });
    }
    if storage.iter().any(|disk| {
        disk.total_bytes > 0
            && u128::from(disk.used_bytes) * 100 / u128::from(disk.total_bytes) >= 90
    }) {
        alerts.push(Alert {
            id: "storage-high".into(),
            level: AlertLevel::Warning,
            title: "Storage headroom is low".into(),
            detail: "One or more mounted filesystems are above 90% usage.".into(),
        });
    }

    Ok(DeviceSnapshot {
        collected_at: Utc::now(),
        synthetic: false,
        identity: observation.identity,
        metrics: observation.metrics,
        probe: Some(observation.metadata),
        storage,
        interfaces,
        services,
        capabilities,
        alerts,
    })
}

fn spi_nor_detected() -> bool {
    fs::read_dir("/sys/class/mtd").is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let id = entry.file_name().to_string_lossy().into_owned();
            id.strip_prefix("mtd").is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix.bytes().all(|byte| byte.is_ascii_digit())
                    && Path::new("/dev").join(&id).exists()
                    && read_trimmed(entry.path().join("type"))
                        .is_some_and(|kind| kind.eq_ignore_ascii_case("nor"))
            })
        })
    })
}

fn demo_snapshot() -> DeviceSnapshot {
    DeviceSnapshot {
        collected_at: Utc::now(),
        synthetic: true,
        probe: None,
        identity: DeviceIdentity {
            id: "demo-rock-5b-01".into(),
            hostname: "lab-rock-5b".into(),
            product: "Radxa ROCK 5B".into(),
            soc: "Rockchip RK3588".into(),
            soc_vendor: Some("Rockchip".into()),
            operating_system: "Radxa OS 2026 (demo)".into(),
            kernel: "6.1.115-rk3588".into(),
            architecture: "aarch64".into(),
            mode: ProbeMode::Demo,
        },
        metrics: MetricSet {
            cpu_percent: Some(31.4),
            load_average: Some([2.51, 1.94, 1.37]),
            memory_used_bytes: Some(5_421_883_392),
            memory_total_bytes: Some(17_179_869_184),
            temperature_c: Some(54.8),
            uptime_seconds: Some(352_842),
        },
        storage: vec![
            StorageMetric { name: "nvme0n1p2".into(), mount_point: "/".into(), used_bytes: 76_826_968_064, total_bytes: 256_060_514_304, removable: false },
            StorageMetric { name: "mmcblk0p1".into(), mount_point: "/boot".into(), used_bytes: 512_753_664, total_bytes: 1_073_741_824, removable: true },
        ],
        interfaces: vec![
            NetworkInterface { name: "eth0".into(), kind: "ethernet".into(), state: "online".into(), address: Some("192.168.88.42".into()), received_bytes: 8_749_302_440, transmitted_bytes: 2_104_506_773 },
            NetworkInterface { name: "wlan0".into(), kind: "wireless".into(), state: "standby".into(), address: None, received_bytes: 410_230_110, transmitted_bytes: 92_105_232 },
        ],
        services: vec![
            ServiceSummary { id: "ssh.service".into(), label: "Remote shell".into(), state: ServiceState::Active, detail: "Listening on :22".into() },
            ServiceSummary { id: "NetworkManager.service".into(), label: "Network manager".into(), state: ServiceState::Active, detail: "2 interfaces managed".into() },
            ServiceSummary { id: "docker.service".into(), label: "Container runtime".into(), state: ServiceState::Inactive, detail: "Installed · stopped".into() },
        ],
        capabilities: vec![
            capability("device-tree", "Device-tree overlays", true, "6 overlays available"),
            capability("gpio", "GPIO", true, "Overlay-aware 40-pin map"),
            capability("video", "Video capture", true, "2 Video4Linux devices"),
            capability("thermal", "Thermal controls", true, "3 zones · step_wise"),
            capability("led", "LED control", true, "2 status LEDs · 1 RGB group"),
            capability("spi-flash", "SPI boot flash", true, "16 MiB MTD device"),
            capability(
                "storage",
                "Storage",
                true,
                &storage_detail(1, 1, 1),
            ),
        ],
        alerts: vec![Alert {
            id: "demo-state".into(),
            level: AlertLevel::Info,
            title: "Synthetic telemetry".into(),
            detail: "This host is not an SBC. Actions are simulated until RSETUP_MODE=live and RSETUP_EXECUTION=live are set on Linux.".into(),
        }],
    }
}

fn capability(id: &str, label: &str, available: bool, available_detail: &str) -> Capability {
    Capability {
        id: id.into(),
        label: label.into(),
        available,
        detail: if available {
            available_detail.into()
        } else {
            "Not detected on this device".into()
        },
    }
}

/// Human-readable detail line for the unified `storage` capability.
///
/// Counts are grouped by type so the hardware matrix card can answer
/// "what storage is on this board" in one glance:
/// `1 NVMe · 1 eMMC · 1 SD`. SD cards are grouped as `MMC/SD` when more
/// than one is present; zero devices yields the not-detected copy.
pub(crate) fn storage_detail(nvme_count: u32, emmc_count: u32, sd_count: u32) -> String {
    let mut parts = Vec::new();
    if nvme_count > 0 {
        parts.push(format!("{nvme_count} NVMe"));
    }
    if emmc_count > 0 {
        parts.push(format!("{emmc_count} eMMC"));
    }
    if sd_count > 0 {
        let label = if sd_count > 1 { "MMC/SD" } else { "SD" };
        parts.push(format!("{sd_count} {label}"));
    }
    if parts.is_empty() {
        "No storage devices detected".to_string()
    } else {
        parts.join(" · ")
    }
}

/// Unified `storage` capability for the hardware matrix.
///
/// Available as soon as one of the three counts is non-zero, so an MMC-only
/// board (eMMC or SD) reports storage exactly like an NVMe-only one; the
/// per-type counts land in [`storage_detail`].
fn storage_capability(nvme_count: u32, emmc_count: u32, sd_count: u32) -> Capability {
    Capability {
        id: "storage".into(),
        label: "Storage".into(),
        available: nvme_count > 0 || emmc_count > 0 || sd_count > 0,
        detail: storage_detail(nvme_count, emmc_count, sd_count),
    }
}

fn probe_service(id: &str, label: &str) -> ServiceSummary {
    let state = command_text("systemctl", &["is-active", id]);
    let parsed = match state.as_deref() {
        Some("active") => ServiceState::Active,
        Some("inactive") => ServiceState::Inactive,
        Some("failed") => ServiceState::Failed,
        _ => ServiceState::Unknown,
    };
    ServiceSummary {
        id: id.into(),
        label: label.into(),
        state: parsed,
        detail: state.unwrap_or_else(|| "systemd state unavailable".into()),
    }
}

fn inspected_capability(
    id: &str,
    label: &str,
    available: bool,
    detail: &str,
    reason: Option<&str>,
) -> Capability {
    let mut result = capability(id, label, available, detail);
    if !available {
        if let Some(reason) = reason {
            result.detail = reason.into();
        }
    }
    result
}

fn probe_interfaces() -> Vec<NetworkInterface> {
    let root = Path::new("/sys/class/net");
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "lo" {
                return None;
            }
            let base = entry.path();
            let state = read_trimmed(base.join("operstate")).unwrap_or_else(|| "unknown".into());
            let received_bytes = read_trimmed(base.join("statistics/rx_bytes"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let transmitted_bytes = read_trimmed(base.join("statistics/tx_bytes"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let kind = if name.starts_with("wl") {
                "wireless"
            } else {
                "ethernet"
            };
            let address = command_text("ip", &["-brief", "-4", "address", "show", "dev", &name])
                .and_then(|line| {
                    line.split_whitespace()
                        .nth(2)
                        .map(|value| value.split('/').next().unwrap_or(value).into())
                });
            Some(NetworkInterface {
                name,
                kind: kind.into(),
                state,
                address,
                received_bytes,
                transmitted_bytes,
            })
        })
        .collect()
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    let mut value = fs::read_to_string(path).ok()?;
    while value.ends_with(['\0', '\n', '\r', ' ']) {
        value.pop();
    }
    Some(value)
}

fn command_text(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_capability_preserves_a_known_backend_reason() {
        let capability = inspected_capability(
            "device-tree",
            "Device-tree overlays",
            false,
            "detected",
            Some("UEFI + DT detected. Overlay configuration is not read or managed yet."),
        );
        assert!(!capability.available);
        assert!(capability.detail.contains("UEFI + DT"));
    }

    #[test]
    fn storage_detail_formats_counts_by_type() {
        assert_eq!(storage_detail(0, 0, 0), "No storage devices detected");
        assert_eq!(storage_detail(1, 0, 0), "1 NVMe");
        assert_eq!(storage_detail(1, 1, 1), "1 NVMe · 1 eMMC · 1 SD");
        assert_eq!(storage_detail(1, 0, 2), "1 NVMe · 2 MMC/SD");
        assert_eq!(storage_detail(0, 2, 0), "2 eMMC");
        assert_eq!(storage_detail(0, 1, 1), "1 eMMC · 1 SD");
    }

    #[test]
    fn demo_snapshot_exposes_storage_capability() {
        let snapshot = demo_snapshot();
        assert!(snapshot.probe.is_none());
        let json = serde_json::to_value(&snapshot).unwrap();
        assert!(json.get("probe").is_none());
        assert_eq!(json["metrics"]["cpuPercent"], serde_json::json!(31.4_f32));
        let restored: DeviceSnapshot = serde_json::from_value(json).unwrap();
        assert_eq!(
            restored.metrics.memory_used_bytes,
            snapshot.metrics.memory_used_bytes
        );
        let storage = snapshot
            .capabilities
            .iter()
            .find(|capability| capability.id == "storage")
            .expect("storage capability present");
        assert!(storage.available);
        assert_eq!(storage.detail, "1 NVMe · 1 eMMC · 1 SD");
        assert!(
            !snapshot
                .capabilities
                .iter()
                .any(|capability| capability.id == "nvme")
        );
    }

    #[test]
    fn storage_capability_is_available_from_counts_alone() {
        let nvme_only = storage_capability(1, 0, 0);
        assert!(nvme_only.available);
        assert_eq!(nvme_only.id, "storage");
        assert_eq!(nvme_only.label, "Storage");
        assert_eq!(nvme_only.detail, "1 NVMe");

        let emmc_only = storage_capability(0, 1, 0);
        assert!(emmc_only.available, "an eMMC-only host must report storage");
        assert_eq!(emmc_only.detail, "1 eMMC");

        let sd_only = storage_capability(0, 0, 1);
        assert!(sd_only.available, "an SD-only host must report storage");
        assert_eq!(sd_only.detail, "1 SD");

        let both_mmc = storage_capability(0, 1, 1);
        assert!(both_mmc.available);
        assert_eq!(both_mmc.detail, "1 eMMC · 1 SD");

        let mixed = storage_capability(1, 1, 0);
        assert!(mixed.available);
        assert_eq!(mixed.detail, "1 NVMe · 1 eMMC");
    }

    #[test]
    fn storage_capability_is_unavailable_without_any_device() {
        let empty = storage_capability(0, 0, 0);
        assert!(
            !empty.available,
            "a host with neither NVMe nor MMC has no storage"
        );
        assert_eq!(empty.id, "storage");
        assert_eq!(empty.detail, "No storage devices detected");
    }

    #[test]
    fn live_storage_capability_mirrors_probe_results() {
        let snapshot = live_snapshot(&mut LocalProbe::default()).expect("live snapshot");
        let counts = crate::deviceinfo_probe::LocalProbe::default()
            .collect()
            .storage_counts;
        let storage = snapshot
            .capabilities
            .iter()
            .find(|capability| capability.id == "storage")
            .expect("storage capability present in live snapshot");
        assert!(
            !snapshot
                .capabilities
                .iter()
                .any(|capability| capability.id == "nvme")
        );
        let expected = storage_capability(counts.0, counts.1, counts.2);
        assert_eq!(storage.id, expected.id);
        assert_eq!(storage.label, expected.label);
        assert_eq!(
            storage.available, expected.available,
            "storage availability must mirror NVMe-or-MMC probe results"
        );
        assert_eq!(
            storage.detail, expected.detail,
            "storage detail must reflect live probe counts"
        );
    }
}
