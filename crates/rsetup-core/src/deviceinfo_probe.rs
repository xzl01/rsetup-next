//! Read-only deviceinfo v2 adapter. No command checks, ioctls or mutation here.
use crate::{DeviceIdentity, MetricSet, ProbeMetadata, ProbeMode, StorageMetric};
use deviceinfo::{
    PlatformReport, Snapshot, StorageInterface, StorageReport, SystemReport, SystemSampleOptions,
    SystemState, ThermalOptions, ThermalReport,
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const IDENTITY_TTL: Duration = Duration::from_secs(30);

/// Window used to establish the CPU baseline of a fresh probe. Utilization is a
/// difference between two samples and deviceinfo never delays implicitly, so a
/// one-shot consumer (CLI `status`, TUI startup, the first HTTP poll) would
/// otherwise report CPU as unknown until its second observation.
const FIRST_SAMPLE_WINDOW: Duration = Duration::from_millis(200);

struct IdentityCache {
    at: Instant,
    fingerprint: [Option<String>; 5],
    system: Snapshot<SystemReport>,
    platform: Snapshot<PlatformReport>,
}

pub(crate) struct LocalProbe {
    root: PathBuf,
    architecture: String,
    identity: Option<IdentityCache>,
    previous: Option<Snapshot<SystemState>>,
    baseline_window: Option<Duration>,
}

pub(crate) struct LocalObservation {
    pub identity: DeviceIdentity,
    pub metrics: MetricSet,
    pub storage: Vec<StorageMetric>,
    pub storage_counts: (u32, u32, u32),
    pub metadata: ProbeMetadata,
}

impl Default for LocalProbe {
    fn default() -> Self {
        Self::new(PathBuf::from("/"), std::env::consts::ARCH.into())
    }
}

impl LocalProbe {
    fn new(root: PathBuf, architecture: String) -> Self {
        Self {
            root,
            architecture,
            identity: None,
            previous: None,
            baseline_window: Some(FIRST_SAMPLE_WINDOW),
        }
    }

    pub(crate) fn invalidate_identity(&mut self) {
        self.identity = None;
        self.previous = None;
    }

    pub(crate) fn collect(&mut self) -> LocalObservation {
        let options = SystemSampleOptions {
            watch: vec!["/".into(), "/boot".into()],
        };
        let mut state = deviceinfo::observe_system(&self.root, &options);
        if self.previous.is_none() {
            if let Some(window) = self.baseline_window {
                // A difference-based CPU sample needs a baseline: keep the first
                // sample as `previous`, wait, then observe again.
                self.previous = Some(state);
                std::thread::sleep(window);
                state = deviceinfo::observe_system(&self.root, &options);
            }
        }
        // Kernel/hostname changes invalidate immediately, even inside the TTL.
        let fingerprint = [
            state.context.finished.boot_id.clone(),
            read_text(&self.root, "proc/sys/kernel/hostname"),
            read_text(&self.root, "proc/sys/kernel/osrelease"),
            state.context.finished.time_namespace.clone(),
            state.context.finished.mount_namespace.clone(),
        ];
        if self.identity.as_ref().is_none_or(|cached| {
            cached.at.elapsed() >= IDENTITY_TTL || cached.fingerprint != fingerprint
        }) {
            self.identity = Some(IdentityCache {
                at: Instant::now(),
                fingerprint,
                system: deviceinfo::inspect_system(&self.root, &self.architecture),
                platform: deviceinfo::inspect_platform(&self.root),
            });
        }
        let cached = self.identity.as_ref().expect("identity populated");
        // Inventory is refreshed on every observation so hotplug is not hidden.
        let inventory = deviceinfo::inspect_storage(&self.root);
        let thermal = deviceinfo::observe_thermal(&self.root, &ThermalOptions::default());
        let (cpu_percent, cpu_unavailable_reason) = match self.previous.as_ref() {
            Some(previous) => match state.cpu_usage_since(previous) {
                Ok(value) => (Some(value as f32), None),
                Err(error) => (None, Some(format!("{error:?}"))),
            },
            None => (None, Some("Awaiting a second system sample".into())),
        };
        let system = &cached.system.data;
        let hostname = system.hostname.clone().unwrap_or_else(|| "unknown".into());
        let product = cached
            .platform
            .data
            .device_tree_model
            .as_ref()
            .map(|v| v.value.clone())
            .or_else(|| {
                cached
                    .platform
                    .data
                    .dmi
                    .product_name
                    .as_ref()
                    .map(|v| v.value.clone())
            })
            .or_else(|| system.cpu.machine_model.clone())
            .unwrap_or_else(|| "Linux SBC".into());
        let id = read_text(&self.root, "etc/machine-id")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("{hostname}-{product}"))
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(16)
            .collect();
        let identity = DeviceIdentity {
            id,
            hostname,
            product,
            soc: system
                .soc
                .model
                .clone()
                .unwrap_or_else(|| "unknown-soc".into()),
            soc_vendor: system.soc.vendor.clone(),
            operating_system: system
                .operating_system
                .as_ref()
                .and_then(|os| os.pretty_name.clone().or_else(|| os.name.clone()))
                .unwrap_or_else(|| "unknown".into()),
            kernel: system
                .kernel_release
                .clone()
                .unwrap_or_else(|| "unknown".into()),
            architecture: system.cpu.arch.clone(),
            mode: ProbeMode::Live,
        };
        let metrics = MetricSet {
            cpu_percent,
            load_average: state
                .data
                .load_average
                .map(|values| values.map(|v| v as f32)),
            memory_used_bytes: state.data.memory.used_bytes(),
            memory_total_bytes: state.data.memory.total_bytes,
            temperature_c: temperature(&thermal.data),
            uptime_seconds: state.data.uptime_seconds.map(|v| v as u64),
        };
        let storage = mounted_metrics(&state.data, &inventory.data);
        let storage_counts = storage_counts(&inventory.data);
        let mut diagnostics = Vec::new();
        for entries in [
            &state.context.diagnostics,
            &cached.system.context.diagnostics,
            &cached.platform.context.diagnostics,
            &cached.platform.data.diagnostics,
            &inventory.context.diagnostics,
            &thermal.context.diagnostics,
            &thermal.data.diagnostics,
        ] {
            for diagnostic in entries {
                if !diagnostics.contains(diagnostic) {
                    diagnostics.push(diagnostic.clone());
                }
            }
        }
        let warnings = [
            &system.warnings,
            &system.soc.warnings,
            &state.data.warnings,
            &inventory.data.warnings,
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
        let metadata = ProbeMetadata {
            provider: "deviceinfo".into(),
            schema_version: state.schema_version,
            context: state.context.clone(),
            identity_context: cached.system.context.clone(),
            platform_context: cached.platform.context.clone(),
            storage_context: inventory.context,
            thermal_context: thermal.context,
            platform: cached.platform.data.clone(),
            diagnostics,
            warnings,
            cpu_unavailable_reason,
        };
        // Keep the complete envelope, not only CPU ticks. Reboot, namespace and
        // sampling-window checks are mandatory for the next difference.
        self.previous = Some(state);
        LocalObservation {
            identity,
            metrics,
            storage,
            storage_counts,
            metadata,
        }
    }
}

fn read_text(root: &Path, relative: &str) -> Option<String> {
    fs::read_to_string(root.join(relative))
        .ok()
        .map(|v| v.trim().to_owned())
}

fn temperature(report: &ThermalReport) -> Option<f32> {
    let maximum = |zones_only: bool| {
        report
            .temperatures
            .iter()
            .filter(|sensor| {
                !zones_only || sensor.origin == deviceinfo::TemperatureOrigin::ThermalZone
            })
            .filter_map(|sensor| sensor.temperature_millicelsius)
            .max()
            .map(|value| value as f32 / 1000.0)
    };
    maximum(true).or_else(|| maximum(false))
}

fn storage_counts(report: &StorageReport) -> (u32, u32, u32) {
    report
        .devices
        .iter()
        .filter(|device| device.partition_number.is_none())
        .fold((0, 0, 0), |(nvme, emmc, sd), device| {
            match device.interface {
                StorageInterface::Nvme => (nvme + 1, emmc, sd),
                StorageInterface::Mmc => (nvme, emmc + 1, sd),
                StorageInterface::Sd => (nvme, emmc, sd + 1),
                _ => (nvme, emmc, sd),
            }
        })
}

fn removable(report: &StorageReport, name: &str, visited: &mut BTreeSet<String>) -> bool {
    if !visited.insert(name.to_owned()) {
        return false;
    }
    report
        .devices
        .iter()
        .find(|device| device.name == name)
        .is_some_and(|device| {
            device.removable == Some(true)
                || device
                    .parent
                    .as_deref()
                    .is_some_and(|parent| removable(report, parent, visited))
                || device
                    .backing_devices
                    .iter()
                    .any(|backing| removable(report, backing, visited))
        })
}

fn mounted_metrics(state: &SystemState, inventory: &StorageReport) -> Vec<StorageMetric> {
    let mut seen = BTreeSet::new();
    state
        .disks
        .iter()
        .filter_map(|usage| {
            // statvfs(/boot) may describe the root filesystem. Pick the actual
            // namespace mount by longest path, then deduplicate that mount only.
            let mount = inventory
                .mounts
                .iter()
                .filter(|mount| usage.path.starts_with(&mount.mount_point))
                .max_by_key(|mount| (mount.mount_point.components().count(), mount.mount_id));
            let mount_point = mount.map(|mount| &mount.mount_point).unwrap_or(&usage.path);
            if !seen.insert(mount_point.clone()) {
                return None;
            }
            let name = mount
                .and_then(|mount| mount.block_device.clone())
                .or_else(|| mount.map(|mount| mount.source.trim_start_matches("/dev/").to_owned()))
                .unwrap_or_else(|| "unknown".into());
            Some(StorageMetric {
                removable: removable(inventory, &name, &mut BTreeSet::new()),
                name,
                mount_point: mount_point.to_string_lossy().into_owned(),
                used_bytes: usage.used_bytes(),
                total_bytes: usage.total_bytes,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use deviceinfo::{CaptureMetadata, ObservationOrigin, SampleContext, SampleStamp};

    const BOOT: &str = "11111111-1111-4111-8111-111111111111";
    const NEXT_BOOT: &str = "22222222-2222-4222-8222-222222222222";

    fn write_at(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn context_at(root: &Path, boot: &str, time: u64, namespace: &str, preserved: bool) {
        write_at(root, deviceinfo::BOOT_ID_INPUT, boot);
        let start = SampleStamp {
            unix_time_ns: Some(1_000_000_000_000 + time),
            boot_time_ns: Some(time),
            boot_id: Some(boot.into()),
            time_namespace: Some(namespace.into()),
            mount_namespace: Some("mnt:[8]".into()),
            boot_clock_resolution_ns: Some(1),
        };
        let mut end = start.clone();
        end.boot_time_ns = Some(time + 1000);
        let metadata = CaptureMetadata {
            schema_version: deviceinfo::SCHEMA_VERSION,
            context: SampleContext::from_bounds(ObservationOrigin::Captured, start, end),
            devices: vec![],
            counters_preserved: preserved,
        };
        write_at(
            root,
            deviceinfo::CONTEXT_FILE,
            &serde_json::to_string(&metadata).unwrap(),
        );
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let fixture = Self(
                std::env::temp_dir().join(format!("rsetup-deviceinfo-{}", uuid::Uuid::new_v4())),
            );
            fixture.write("proc/cpuinfo", "processor : 0\nCPU implementer : 0x51\n");
            fixture.write("proc/sys/kernel/hostname", "dragon-q8b\n");
            fixture.write("proc/sys/kernel/osrelease", "6.12-test\n");
            fixture.write(
                "etc/os-release",
                "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nID=debian\n",
            );
            fixture.write("etc/machine-id", "abc0123\n");
            fixture.write(
                "proc/meminfo",
                "MemTotal: 1000 kB\nMemAvailable: 400 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n",
            );
            fixture.write("proc/loadavg", "9.0 8.0 7.0 1/10 10\n");
            fixture.write("proc/uptime", "42.0 10.0\n");
            fixture.write("proc/stat", "cpu 10 0 0 90 0 0 0 0\n");
            fixture.write("sys/firmware/devicetree/base/model", "Radxa Dragon Q8B\0");
            fixture.write(
                "sys/firmware/devicetree/base/compatible",
                "radxa,dragon-q8b\0qcom,sc8280xp\0",
            );
            fixture.write("sys/firmware/efi/fw_platform_size", "64\n");
            fixture.write("sys/class/thermal/thermal_zone0/type", "soc-thermal\n");
            fixture.write("sys/class/thermal/thermal_zone0/temp", "54000\n");
            fixture.write("proc/self/mountinfo", "");
            fs::create_dir_all(fixture.0.join("sys/class/block")).unwrap();
            fixture.context(BOOT, 1_000_000_000, "time:[7]", true);
            fixture
        }
        fn write(&self, relative: &str, text: &str) {
            write_at(&self.0, relative, text);
        }
        fn context(&self, boot: &str, time: u64, namespace: &str, preserved: bool) {
            context_at(&self.0, boot, time, namespace, preserved);
        }
        fn probe(&self) -> LocalProbe {
            let mut probe = LocalProbe::new(self.0.clone(), "aarch64".into());
            // Fixtures have no advancing /proc/stat, so the baseline window adds
            // latency without changing any assertion. Tests that exercise it opt in.
            probe.baseline_window = None;
            probe
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn q8b_preserves_uefi_and_dt_and_uses_source_identity() {
        use deviceinfo::Exposure;
        let fixture = Fixture::new();
        let observed = fixture.probe().collect();
        assert_eq!(observed.identity.product, "Radxa Dragon Q8B");
        assert_eq!(observed.identity.soc.to_ascii_uppercase(), "SC8280XP");
        assert_eq!(observed.identity.soc_vendor.as_deref(), Some("Qualcomm"));
        assert_eq!(observed.identity.hostname, "dragon-q8b");
        assert_eq!(observed.identity.kernel, "6.12-test");
        assert_eq!(
            observed.identity.operating_system,
            "Debian GNU/Linux 12 (bookworm)"
        );
        assert_eq!(observed.identity.architecture, "aarch64");
        assert_eq!(observed.metadata.platform.uefi.exposure, Exposure::Exposed);
        assert_eq!(
            observed.metadata.platform.device_tree.exposure,
            Exposure::Exposed
        );
        assert_eq!(
            observed.metadata.platform.acpi.exposure,
            Exposure::NotExposed
        );
        assert_eq!(
            observed.metadata.context.origin,
            ObservationOrigin::Captured
        );
        assert_eq!(observed.metrics.memory_total_bytes, Some(1000 * 1024));
        assert_eq!(observed.metrics.memory_used_bytes, Some(600 * 1024));
        assert_eq!(observed.metrics.temperature_c, Some(54.0));
        assert!(
            observed.storage.is_empty(),
            "fixture must not report host filesystems"
        );
        assert!(
            observed
                .metadata
                .diagnostics
                .iter()
                .any(|d| d.code == deviceinfo::DiagnosticCode::Unsupported)
        );
    }

    #[test]
    fn cpu_uses_full_snapshot_difference_not_load_average() {
        let fixture = Fixture::new();
        let mut probe = fixture.probe();
        assert_eq!(probe.collect().metrics.cpu_percent, None);
        fixture.write("proc/stat", "cpu 60 0 0 140 0 0 0 0\n");
        fixture.context(BOOT, 2_000_000_000, "time:[7]", true);
        let next = probe.collect();
        assert_eq!(next.metrics.cpu_percent, Some(50.0));
        assert_eq!(next.metrics.load_average.unwrap()[0], 9.0);
        assert_eq!(next.metadata.cpu_unavailable_reason, None);
        let json = serde_json::to_value(&next.metadata).unwrap();
        assert_eq!(json["context"]["started"]["boot_time_ns"], "2000000000");
    }

    #[test]
    fn independent_probes_and_demo_reset_do_not_reuse_cpu_baselines() {
        let fixture = Fixture::new();
        let mut first = fixture.probe();
        first.collect();
        fixture.write("proc/stat", "cpu 60 0 0 140 0 0 0 0\n");
        fixture.context(BOOT, 2_000_000_000, "time:[7]", true);
        assert_eq!(first.collect().metrics.cpu_percent, Some(50.0));
        assert_eq!(fixture.probe().collect().metrics.cpu_percent, None);
        first.invalidate_identity();
        assert_eq!(first.collect().metrics.cpu_percent, None);
        fixture.context(BOOT, 3_000_000_000, "time:[7]", true);
        let unchanged = first.collect();
        assert_eq!(unchanged.metrics.cpu_percent, None);
        assert_eq!(
            unchanged.metadata.cpu_unavailable_reason.as_deref(),
            Some("NoCounterProgress")
        );
    }

    #[test]
    fn one_shot_collect_establishes_a_cpu_baseline_inside_the_window() {
        let fixture = Fixture::new();
        let mut probe = fixture.probe();
        // Production behaviour: a fresh probe must not need a second call.
        // Generous margins keep the two samples on either side of the change.
        probe.baseline_window = Some(Duration::from_millis(1000));
        let root = fixture.0.clone();
        let advancing = std::thread::spawn(move || {
            // Land the counter change inside the adapter's sampling window.
            std::thread::sleep(Duration::from_millis(200));
            write_at(&root, "proc/stat", "cpu 60 0 0 140 0 0 0 0\n");
            context_at(&root, BOOT, 2_000_000_000, "time:[7]", true);
        });
        let observed = probe.collect();
        advancing.join().unwrap();
        assert_eq!(
            observed.metrics.cpu_percent,
            Some(50.0),
            "reason: {:?}",
            observed.metadata.cpu_unavailable_reason
        );
        assert_eq!(observed.metadata.cpu_unavailable_reason, None);
    }

    #[test]
    fn missing_system_metrics_serialize_as_null_not_zero() {
        let fixture = Fixture::new();
        fixture.write("proc/meminfo", "invalid\n");
        fixture.write("proc/loadavg", "NaN 1.0 2.0\n");
        fixture.write("proc/uptime", "invalid\n");
        let metrics = fixture.probe().collect().metrics;
        assert_eq!(metrics.memory_percent(), None);
        let json = serde_json::to_value(metrics).unwrap();
        for field in [
            "cpuPercent",
            "loadAverage",
            "memoryUsedBytes",
            "memoryTotalBytes",
            "uptimeSeconds",
        ] {
            assert!(json[field].is_null(), "{field}");
        }
    }

    #[test]
    fn reboot_namespace_reset_and_frozen_capture_do_not_produce_percent() {
        for case in ["boot", "namespace", "reset", "frozen", "scrubbed"] {
            let fixture = Fixture::new();
            let mut probe = fixture.probe();
            probe.collect();
            fixture.write(
                "proc/stat",
                if case == "reset" {
                    "cpu 1 0 0 1 0 0 0 0\n"
                } else {
                    "cpu 60 0 0 140 0 0 0 0\n"
                },
            );
            fixture.context(
                if case == "boot" { NEXT_BOOT } else { BOOT },
                if case == "frozen" {
                    1_000_000_000
                } else {
                    2_000_000_000
                },
                if case == "namespace" {
                    "time:[9]"
                } else {
                    "time:[7]"
                },
                case != "scrubbed",
            );
            let next = probe.collect();
            assert_eq!(next.metrics.cpu_percent, None, "{case}");
            assert!(next.metadata.cpu_unavailable_reason.is_some(), "{case}");
        }
    }

    #[test]
    fn identity_cache_invalidates_on_hostname_kernel_boot_and_ttl() {
        let fixture = Fixture::new();
        let mut probe = fixture.probe();
        probe.collect();
        fixture.write("sys/firmware/devicetree/base/model", "changed-product\0");
        assert_eq!(
            probe.collect().identity.product,
            "Radxa Dragon Q8B",
            "identity is cached"
        );
        fixture.write("proc/sys/kernel/hostname", "renamed\n");
        assert_eq!(probe.collect().identity.product, "changed-product");
        assert_eq!(probe.collect().identity.hostname, "renamed");
        fixture.write("proc/sys/kernel/osrelease", "new-kernel\n");
        assert_eq!(probe.collect().identity.kernel, "new-kernel");
        fixture.write("sys/firmware/devicetree/base/model", "after-reboot\0");
        fixture.context(NEXT_BOOT, 1_000_000_000, "time:[7]", true);
        assert_eq!(probe.collect().identity.product, "after-reboot");
        fixture.write("sys/firmware/devicetree/base/model", "after-ttl\0");
        probe.identity.as_mut().unwrap().at = Instant::now() - IDENTITY_TTL;
        assert_eq!(probe.collect().identity.product, "after-ttl");
    }

    #[test]
    fn unsupported_optional_inputs_remain_diagnostic_not_fatal() {
        let fixture = Fixture::new();
        fixture.write("sys/class/thermal/thermal_zone0/temp", "not-a-number\n");
        fixture.write("proc/stat", "not-cpu\n");
        fs::remove_file(fixture.0.join(deviceinfo::CONTEXT_FILE)).unwrap();
        let mut probe = fixture.probe();
        let value = probe.collect();
        assert_eq!(value.metrics.temperature_c, None);
        assert!(!value.metadata.context.consistent);
        assert!(
            value
                .metadata
                .diagnostics
                .iter()
                .any(|d| d.code == deviceinfo::DiagnosticCode::InvalidData)
        );
        assert!(!value.metadata.warnings.is_empty());
        assert_eq!(probe.collect().metrics.cpu_percent, None);
    }

    #[test]
    fn faulted_hwmon_sensor_is_not_used_as_a_temperature() {
        let fixture = Fixture::new();
        fixture.write("sys/class/thermal/thermal_zone0/temp", "invalid\n");
        fixture.write("sys/class/hwmon/hwmon0/name", "sensor\n");
        fixture.write("sys/class/hwmon/hwmon0/temp1_input", "100000\n");
        fixture.write("sys/class/hwmon/hwmon0/temp1_fault", "1\n");
        assert_eq!(fixture.probe().collect().metrics.temperature_c, None);
        fixture.write("sys/class/hwmon/hwmon0/temp1_fault", "0\n");
        fixture.write("sys/class/hwmon/hwmon0/temp1_input", "-1000\n");
        assert_eq!(fixture.probe().collect().metrics.temperature_c, Some(-1.0));
    }

    #[test]
    fn soc_models_are_not_sbc_names_and_conflicting_vendors_stay_unknown() {
        let fixture = Fixture::new();
        for (compatible, expected, vendor) in [
            ("radxa,rock-5b\0rockchip,rk3588\0", "RK3588", "Rockchip"),
            ("radxa,orion-o6\0cix,sky1\0", "SKY1", "CIX"),
            (
                "radxa,cubie-a5e\0allwinner,sun55i-a527\0",
                "SUN55I-A527",
                "Allwinner",
            ),
        ] {
            fixture.write("sys/firmware/devicetree/base/compatible", compatible);
            let identity = fixture.probe().collect().identity;
            assert_eq!(identity.soc.to_ascii_uppercase(), expected);
            assert_eq!(identity.soc_vendor.as_deref(), Some(vendor));
        }
        fixture.write("sys/devices/soc0/family", "Qualcomm\n");
        let value = fixture.probe().collect();
        assert_eq!(value.identity.soc_vendor, None);
        assert!(
            value
                .metadata
                .warnings
                .iter()
                .any(|warning| warning.contains("Conflicting"))
        );
    }

    #[test]
    fn mounts_use_major_minor_and_parent_removable_and_deduplicate_boot() {
        let fixture = Fixture::new();
        for (name, dev, removable) in [("sda", "8:0", "1"), ("sda1", "8:1", "0")] {
            fixture.write(&format!("sys/class/block/{name}/dev"), dev);
            fixture.write(&format!("sys/class/block/{name}/size"), "1000\n");
            fixture.write(&format!("sys/class/block/{name}/removable"), removable);
        }
        fixture.write("sys/class/block/sda1/partition", "1\n");
        fixture.write("sys/class/block/sda/sda1/partition", "1\n");
        fixture.write("proc/self/mountinfo", "20 1 8:1 / / rw - ext4 /dev/root rw\n21 20 8:1 / /data\\040space rw - ext4 /dev/mapper/alias rw\n");
        let inventory = deviceinfo::inspect_storage(&fixture.0).data;
        assert_eq!(inventory.mounts[0].block_device.as_deref(), Some("sda1"));
        assert_eq!(inventory.mounts[1].mount_point, Path::new("/data space"));
        let mut state =
            deviceinfo::observe_system(&fixture.0, &SystemSampleOptions::default()).data;
        state.disks = ["/", "/boot", "/data space"]
            .map(|path| deviceinfo::DiskUsage {
                path: path.into(),
                total_bytes: 100,
                free_bytes: 30,
                available_bytes: 20,
            })
            .into();
        let metrics = mounted_metrics(&state, &inventory);
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].name, "sda1");
        assert_eq!(
            metrics[0].used_bytes, 70,
            "reserved free blocks are not used bytes"
        );
        assert!(metrics.iter().all(|metric| metric.removable));
    }

    #[test]
    fn storage_hotplug_updates_without_identity_cache_refresh() {
        let fixture = Fixture::new();
        let mut probe = fixture.probe();
        assert_eq!(probe.collect().storage_counts, (0, 0, 0));
        fixture.write("sys/class/block/nvme0n1/dev", "259:0\n");
        fixture.write("sys/class/block/nvme0n1/size", "1000\n");
        fixture.write("sys/class/block/nvme0n1/device/transport", "pcie\n");
        assert_eq!(probe.collect().storage_counts, (1, 0, 0));
    }
}
