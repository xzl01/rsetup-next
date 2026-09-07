use crate::{
    Alert, AlertLevel, Capability, DeviceIdentity, DeviceSnapshot, MetricSet, NetworkInterface,
    ProbeMode, ServiceState, ServiceSummary, StorageMetric, hardware::HardwareManager,
};
use anyhow::{Context, Result};
use chrono::Utc;
use std::{
    collections::{BTreeSet, HashMap},
    env, fs,
    path::Path,
    process::Command,
};

pub fn collect_snapshot(requested_mode: ProbeMode) -> Result<DeviceSnapshot> {
    let mode = resolve_mode(requested_mode);
    match mode {
        ProbeMode::Demo => Ok(demo_snapshot()),
        ProbeMode::Live | ProbeMode::Auto => live_snapshot(),
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

fn live_snapshot() -> Result<DeviceSnapshot> {
    let hostname = read_trimmed("/etc/hostname").unwrap_or_else(|| "localhost".into());
    let product = read_trimmed("/proc/device-tree/model")
        .or_else(|| read_trimmed("/sys/devices/virtual/dmi/id/product_name"))
        .unwrap_or_else(|| "Linux SBC".into());
    let compatibles = read_nul_lines("/proc/device-tree/compatible");
    let soc = detect_soc(&compatibles).unwrap_or_else(|| "unknown-soc".into());
    let soc_vendor = detect_soc_vendor(&compatibles, &soc).map(str::to_owned);
    let os_release = parse_key_values("/etc/os-release");
    let operating_system = os_release
        .get("PRETTY_NAME")
        .cloned()
        .unwrap_or_else(|| "Linux".into());
    let kernel = command_text("uname", &["-r"]).unwrap_or_else(|| "unknown".into());
    let architecture = command_text("uname", &["-m"]).unwrap_or_else(|| env::consts::ARCH.into());
    let meminfo = parse_meminfo();
    let memory_total_bytes = meminfo.get("MemTotal").copied().unwrap_or(0) * 1024;
    let memory_available = meminfo.get("MemAvailable").copied().unwrap_or(0) * 1024;
    let memory_used_bytes = memory_total_bytes.saturating_sub(memory_available);
    let load_average = parse_load_average();
    let cpu_count = read_trimmed("/proc/cpuinfo")
        .map(|value| {
            value
                .lines()
                .filter(|line| line.starts_with("processor"))
                .count()
        })
        .unwrap_or(1)
        .max(1) as f32;
    let cpu_percent = ((load_average[0] / cpu_count) * 100.0).clamp(0.0, 100.0);
    let uptime_seconds = read_trimmed("/proc/uptime")
        .and_then(|value| value.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    let temperature_c = read_temperature();
    let storage = probe_storage();
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
    if storage
        .iter()
        .any(|disk| disk.total_bytes > 0 && disk.used_bytes * 100 / disk.total_bytes >= 90)
    {
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
        identity: DeviceIdentity {
            id: stable_device_id(&hostname, &product),
            hostname,
            product,
            soc,
            soc_vendor,
            operating_system,
            kernel,
            architecture,
            mode: ProbeMode::Live,
        },
        metrics: MetricSet {
            cpu_percent,
            load_average,
            memory_used_bytes,
            memory_total_bytes,
            temperature_c,
            uptime_seconds,
        },
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
            cpu_percent: 31.4,
            load_average: [2.51, 1.94, 1.37],
            memory_used_bytes: 5_421_883_392,
            memory_total_bytes: 17_179_869_184,
            temperature_c: Some(54.8),
            uptime_seconds: 352_842,
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

fn probe_storage() -> Vec<StorageMetric> {
    let Some(output) = command_text("df", &["-Pk", "/", "/boot"]) else {
        return Vec::new();
    };
    parse_storage(&output, Path::new("/"))
}

fn parse_storage(output: &str, root: &Path) -> Vec<StorageMetric> {
    let mut seen = BTreeSet::new();
    output
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 6 {
                return None;
            }
            let metric = StorageMetric {
                name: fields[0].trim_start_matches("/dev/").into(),
                mount_point: fields[5].into(),
                used_bytes: fields[2].parse::<u64>().ok()?.saturating_mul(1024),
                total_bytes: fields[1].parse::<u64>().ok()?.saturating_mul(1024),
                removable: block_removable(root, fields[0]),
            };
            seen.insert((fields[0], fields[5])).then_some(metric)
        })
        .collect()
}

fn block_removable(root: &Path, device: &str) -> bool {
    let Some(name) = device.strip_prefix("/dev/") else {
        return false;
    };
    // Resolve aliases such as /dev/root and /dev/mapper/* when available.
    let resolved = fs::canonicalize(root.join("dev").join(name)).ok();
    let name = resolved
        .as_ref()
        .and_then(|path| path.file_name())
        .or_else(|| Path::new(name).file_name());
    let Some(name) = name else {
        return false;
    };
    let Ok(mut sysfs) = fs::canonicalize(root.join("sys/class/block").join(name)) else {
        return false;
    };
    // A partition has no removable attribute of its own; query its parent disk.
    if sysfs.join("partition").is_file() {
        sysfs.pop();
    }
    read_trimmed(sysfs.join("removable")).as_deref() == Some("1")
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

fn read_temperature() -> Option<f32> {
    let entries = fs::read_dir("/sys/class/thermal").ok()?;
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| read_trimmed(entry.path().join("temp")))
        .filter_map(|value| value.parse::<f32>().ok())
        .map(|value| {
            if value > 1000.0 {
                value / 1000.0
            } else {
                value
            }
        })
        .max_by(f32::total_cmp)
}

fn parse_meminfo() -> HashMap<String, u64> {
    read_trimmed("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let number = value.split_whitespace().next()?.parse().ok()?;
            Some((key.into(), number))
        })
        .collect()
}

fn parse_load_average() -> [f32; 3] {
    let values: Vec<f32> = read_trimmed("/proc/loadavg")
        .unwrap_or_default()
        .split_whitespace()
        .take(3)
        .filter_map(|value| value.parse().ok())
        .collect();
    [
        *values.first().unwrap_or(&0.0),
        *values.get(1).unwrap_or(&0.0),
        *values.get(2).unwrap_or(&0.0),
    ]
}

fn parse_key_values(path: impl AsRef<Path>) -> HashMap<String, String> {
    read_trimmed(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.into(), value.trim_matches('"').into()))
        })
        .collect()
}

fn stable_device_id(hostname: &str, product: &str) -> String {
    let machine_id = read_trimmed("/etc/machine-id").unwrap_or_default();
    let seed = if machine_id.is_empty() {
        format!("{hostname}-{product}")
    } else {
        machine_id
    };
    seed.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(16)
        .collect()
}

fn detect_soc_vendor<'a>(compatibles: &'a [String], soc: &'a str) -> Option<&'static str> {
    compatibles
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(soc))
        .find_map(|value| {
            let value = value.to_ascii_lowercase();
            if value.starts_with("rockchip,")
                || value.starts_with("rockchip ")
                || value.starts_with("rk35")
            {
                Some("Rockchip")
            } else if value.starts_with("allwinner,")
                || value.starts_with("allwinner ")
                || value.starts_with("sunxi")
                || value.starts_with("sun50")
            {
                Some("Allwinner")
            } else if value.starts_with("cix,") || value.starts_with("cix ") {
                Some("CIX")
            } else if value.starts_with("qcom,")
                || value.starts_with("qualcomm")
                || value.starts_with("snapdragon")
                || value.starts_with("qcs")
            {
                Some("Qualcomm")
            } else if value.starts_with("amlogic,") || value.starts_with("amlogic ") {
                Some("Amlogic")
            } else if value.starts_with("brcm,")
                || value.starts_with("broadcom")
                || value.starts_with("bcm")
            {
                Some("Broadcom")
            } else if value.starts_with("mediatek,")
                || value.starts_with("mediatek ")
                || value.starts_with("mtk")
            {
                Some("MediaTek")
            } else if value.starts_with("nvidia,")
                || value.starts_with("nvidia ")
                || value.starts_with("tegra")
            {
                Some("NVIDIA")
            } else if value.starts_with("nxp,")
                || value.starts_with("nxp ")
                || value.starts_with("fsl,")
                || value.starts_with("imx")
            {
                Some("NXP")
            } else if value.starts_with("starfive,")
                || value.starts_with("starfive ")
                || value.starts_with("jh71")
            {
                Some("StarFive")
            } else if value.starts_with("sophgo,")
                || value.starts_with("sophgo ")
                || value.starts_with("cv18")
            {
                Some("Sophgo")
            } else {
                None
            }
        })
}

fn detect_soc(compatibles: &[String]) -> Option<String> {
    // DT compatibles run from specific (SBC) to general (SoC). Do not use an
    // arbitrary board-vendor suffix as the SoC, and prefer the general SoC entry.
    compatibles.iter().rev().find_map(|value| {
        let (vendor, id) = value.split_once(',')?;
        let family_prefixes: &[&str] = match vendor.to_ascii_lowercase().as_str() {
            "qcom" => &["sc", "sm", "qcs", "qcm", "sdm", "msm", "apq", "ipq", "sa"],
            "rockchip" => &["rk", "rv", "px"],
            "allwinner" => &["sun"],
            "cix" => &["sky", "p"],
            "amlogic" => &["meson", "a", "s", "t"],
            "brcm" => &["bcm"],
            "mediatek" => &["mt"],
            "nvidia" => &["tegra"],
            "fsl" | "nxp" => &["imx", "ls", "lx"],
            "starfive" => &["jh"],
            "sophgo" => &["cv", "sg", "bm"],
            "spacemit" => &["k"],
            _ => return None,
        };
        let id = id.to_ascii_lowercase();
        family_prefixes
            .iter()
            .any(|prefix| {
                id.strip_prefix(prefix).is_some_and(|suffix| {
                    suffix.starts_with(|ch: char| ch.is_ascii_digit())
                        || (*prefix == "meson" && suffix.starts_with('-'))
                })
            })
            .then(|| id.to_ascii_uppercase())
    })
}

fn read_nul_lines(path: impl AsRef<Path>) -> Vec<String> {
    fs::read(path)
        .ok()
        .map(|bytes| {
            bytes
                .split(|byte| *byte == 0)
                .filter(|value| !value.is_empty())
                .map(|value| String::from_utf8_lossy(value).into_owned())
                .collect()
        })
        .unwrap_or_default()
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

#[allow(dead_code)]
fn require_file(path: &str) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("unable to read {path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soc_identity_uses_soc_compatible_instead_of_the_sbc_name() {
        for (values, expected) in [
            (vec!["radxa,dragon-q8b", "qcom,sc8280xp"], "SC8280XP"),
            (vec!["qcom,sc8280xp-crd", "qcom,sc8280xp"], "SC8280XP"),
            (vec!["radxa,rock-5b", "rockchip,rk3588"], "RK3588"),
            (vec!["radxa,orion-o6", "cix,sky1"], "SKY1"),
            (
                vec!["radxa,zero", "amlogic,g12a", "amlogic,meson-g12a"],
                "MESON-G12A",
            ),
            (
                vec!["radxa,cubie-a5e", "allwinner,sun55i-a527"],
                "SUN55I-A527",
            ),
        ] {
            let values = values.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(detect_soc(&values).as_deref(), Some(expected));
        }
        assert_eq!(detect_soc(&["radxa,dragon-q8b".into()]), None);
        assert_eq!(detect_soc(&["qcom,unknown-board".into()]), None);
        assert_eq!(detect_soc(&[]), None);
    }

    #[test]
    fn storage_deduplicates_mounts_and_reads_parent_disk_removable_attribute() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("rsetup-storage-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("sys/class/block")).unwrap();
        for (disk, partition, removable) in [
            ("sda", "sda3", "0"),
            ("sdb", "sdb1", "1"),
            ("mmcblk0", "mmcblk0p1", "0"),
        ] {
            let disk_dir = root.join("sys/devices/block").join(disk);
            fs::create_dir_all(disk_dir.join(partition)).unwrap();
            fs::write(disk_dir.join("removable"), removable).unwrap();
            fs::write(disk_dir.join(partition).join("partition"), "1").unwrap();
            symlink(&disk_dir, root.join("sys/class/block").join(disk)).unwrap();
            symlink(
                disk_dir.join(partition),
                root.join("sys/class/block").join(partition),
            )
            .unwrap();
        }
        let output = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda3 100 25 75 25% /\n/dev/sda3 100 25 75 25% /\n/dev/sdb1 200 50 150 25% /boot\n/dev/mmcblk0p1 40 10 30 25% /config\nmalformed\n";
        let storage = parse_storage(output, &root);
        assert_eq!(storage.len(), 3);
        assert_eq!(storage[0].mount_point, "/");
        assert_eq!(storage[0].used_bytes, 25 * 1024);
        assert!(!storage[0].removable);
        assert!(storage[1].removable);
        assert!(!storage[2].removable);
        assert!(block_removable(&root, "/dev/sdb"));
        assert!(!block_removable(&root, "tmpfs"));
        assert!(!block_removable(&root, "/dev/unknown"));
        fs::remove_dir_all(root).unwrap();
    }

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
    fn detects_vendor_after_board_compatible() {
        let compatibles = vec!["radxa,rock-5b".into(), "rockchip,rk3588".into()];
        assert_eq!(detect_soc_vendor(&compatibles, "rock-5b"), Some("Rockchip"));
    }

    #[test]
    fn detects_common_vendor_from_soc_fallback() {
        assert_eq!(detect_soc_vendor(&[], "sun50i-h616"), Some("Allwinner"));
        assert_eq!(detect_soc_vendor(&[], "qcs8550"), Some("Qualcomm"));
    }
}
