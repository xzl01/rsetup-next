use crate::model::{
    HealthState, NvmeDevice, NvmeSmartLog, NvmeStatus, TelemetryError, TelemetryErrorKind,
    TelemetryReadState, TelemetryStatus,
};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub mod sys;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NvmeError {
    #[error("Invalid SMART buffer length: expected {expected}, got {actual}")]
    InvalidBufferLength { expected: usize, actual: usize },
    #[error("Device not supported: {0}")]
    NotSupported(String),
    #[error("NVMe command status error: {0:#x}")]
    CommandStatus(i32),
    #[error("NVMe I/O errno: {0}")]
    IoCode(i32),
    #[error("I/O error: {0}")]
    Io(String),
}

/// NVMe device and controller manager.
#[derive(Debug, Clone)]
pub struct NvmeManager {
    sysfs_root: PathBuf,
}

impl Default for NvmeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl NvmeManager {
    /// Configure sysfs root for NVMe manager without performing storage I/O.
    pub fn probe_and_init(sysfs_root: Option<&Path>) -> Self {
        let root = sysfs_root
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));

        Self { sysfs_root: root }
    }

    /// Create default instance probing `/`.
    pub fn new() -> Self {
        Self::probe_and_init(None)
    }

    /// Return sysfs root path.
    pub fn sysfs_root(&self) -> &Path {
        &self.sysfs_root
    }

    /// Probe `/sys/class/nvme` under the given root.
    pub fn probe_sysfs(root: &Path) -> Vec<String> {
        Self::try_probe_sysfs(root).unwrap_or_default()
    }

    /// Try to probe `/sys/class/nvme` under the given root, returning any I/O error.
    pub fn try_probe_sysfs(root: &Path) -> Result<Vec<String>, NvmeError> {
        let nvme_class_dir = if root == Path::new("/") {
            PathBuf::from("/sys/class/nvme")
        } else {
            root.join("sys/class/nvme")
        };

        if !nvme_class_dir.exists() {
            return Ok(Vec::new());
        }

        let entries = std::fs::read_dir(&nvme_class_dir).map_err(|e| {
            if let Some(code) = e.raw_os_error() {
                NvmeError::IoCode(code)
            } else {
                NvmeError::Io(e.to_string())
            }
        })?;

        let mut devices = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| {
                if let Some(code) = e.raw_os_error() {
                    NvmeError::IoCode(code)
                } else {
                    NvmeError::Io(e.to_string())
                }
            })?;
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if let Some(suffix) = name.strip_prefix("nvme") {
                if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                    devices.push(name.to_string());
                }
            }
        }
        devices.sort();
        Ok(devices)
    }

    pub(crate) fn status_with(
        &self,
        reader: &dyn Fn(&str) -> Result<[u8; 512], NvmeError>,
    ) -> Result<NvmeStatus, NvmeError> {
        let devices = Self::try_probe_sysfs(&self.sysfs_root)?;
        if devices.is_empty() {
            return Ok(NvmeStatus {
                initialized: false,
                devices: Vec::new(),
                message: Some("No NVMe controller detected in system".into()),
            });
        }

        let nvme_devices = devices
            .into_iter()
            .map(|name| {
                sys::read_controller_sysfs_with(&self.sysfs_root, &name, reader).unwrap_or_else(
                    |_| NvmeDevice {
                        path: format!("/dev/{}", name),
                        name,
                        model: String::new(),
                        serial: String::new(),
                        firmware: String::new(),
                        total_bytes: 0,
                        smart: None,
                        telemetry: TelemetryStatus {
                            state: TelemetryReadState::Unavailable,
                            error: Some(TelemetryError {
                                kind: TelemetryErrorKind::Io,
                                code: None,
                            }),
                        },
                        health_state: HealthState::Unknown,
                    },
                )
            })
            .collect();

        Ok(NvmeStatus {
            initialized: true,
            devices: nvme_devices,
            message: None,
        })
    }

    pub fn status(&self) -> Result<NvmeStatus, NvmeError> {
        if self.sysfs_root == Path::new("/") {
            self.status_with(&sys::read_smart_log_raw)
        } else {
            let fixture_reader = |_dev: &str| -> Result<[u8; 512], NvmeError> {
                Err(NvmeError::NotSupported(
                    "fixture requires an injected reader".to_string(),
                ))
            };
            self.status_with(&fixture_reader)
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.status().map(|s| s.initialized).unwrap_or(false)
    }
}

/// Parse warning flags according to NVMe specification (SMART / Health Information Log byte 0).
fn parse_warning_flags(mask: u8) -> Vec<String> {
    let mut flags = Vec::new();
    if mask & (1 << 0) != 0 {
        flags.push("available_spare_below_threshold".to_string());
    }
    if mask & (1 << 1) != 0 {
        flags.push("temperature_exceeded".to_string());
    }
    if mask & (1 << 2) != 0 {
        flags.push("reliability_degraded".to_string());
    }
    if mask & (1 << 3) != 0 {
        flags.push("read_only".to_string());
    }
    if mask & (1 << 4) != 0 {
        flags.push("volatile_memory_backup_failed".to_string());
    }
    if mask & (1 << 5) != 0 {
        flags.push("persistent_memory_read_only".to_string());
    }
    flags
}

fn read_u128_le(slice: &[u8]) -> u128 {
    let mut arr = [0u8; 16];
    arr.copy_from_slice(&slice[..16]);
    u128::from_le_bytes(arr)
}

fn read_u128_as_u64_saturating(slice: &[u8]) -> u64 {
    let val = read_u128_le(slice);
    if val > u64::MAX as u128 {
        u64::MAX
    } else {
        val as u64
    }
}

pub fn parse_smart_log(buf: &[u8; 512]) -> Result<NvmeSmartLog, NvmeError> {
    let critical_warning = buf[0];
    let warning_flags = parse_warning_flags(critical_warning);

    let kelvin = u16::from_le_bytes([buf[1], buf[2]]);
    let temperature_c = if kelvin == 0 {
        0.0
    } else {
        kelvin as f32 - 273.15
    };

    let available_spare_percent = buf[3];
    let spare_threshold_percent = buf[4];
    let percentage_used = buf[5];

    // Data units read: 128-bit value, in units of 1000 * 512 bytes (512,000 bytes)
    let units_read = read_u128_le(&buf[32..48]);
    let data_read_bytes_128 = units_read.saturating_mul(1000 * 512);
    let data_read_bytes = if data_read_bytes_128 > u64::MAX as u128 {
        u64::MAX
    } else {
        data_read_bytes_128 as u64
    };

    // Data units written: 128-bit value, in units of 1000 * 512 bytes
    let units_written = read_u128_le(&buf[48..64]);
    let data_written_bytes_128 = units_written.saturating_mul(1000 * 512);
    let data_written_bytes = if data_written_bytes_128 > u64::MAX as u128 {
        u64::MAX
    } else {
        data_written_bytes_128 as u64
    };

    let host_read_commands = read_u128_as_u64_saturating(&buf[64..80]);
    let host_write_commands = read_u128_as_u64_saturating(&buf[80..96]);
    let power_on_hours = read_u128_as_u64_saturating(&buf[128..144]);
    let unsafe_shutdowns = read_u128_as_u64_saturating(&buf[144..160]);
    let media_errors = read_u128_as_u64_saturating(&buf[160..176]);
    let num_err_log_entries = read_u128_as_u64_saturating(&buf[176..192]);

    Ok(NvmeSmartLog {
        critical_warning,
        warning_flags,
        temperature_c,
        available_spare_percent,
        spare_threshold_percent,
        percentage_used,
        data_read_bytes,
        data_written_bytes,
        host_read_commands,
        host_write_commands,
        power_on_hours,
        unsafe_shutdowns,
        media_errors,
        num_err_log_entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_contract_nvme_error_injections_preserve_metadata_and_set_telemetry() {
        let root =
            std::env::temp_dir().join(format!("rsetup-nvme-contract-{}", uuid::Uuid::new_v4()));
        let ctrl_dir = root.join("sys/class/nvme/nvme0");
        std::fs::create_dir_all(&ctrl_dir).expect("create ctrl_dir");
        std::fs::write(ctrl_dir.join("model"), "FIXTURE SSD\n").unwrap();
        std::fs::write(ctrl_dir.join("serial"), "SERIAL123\n").unwrap();
        std::fs::write(ctrl_dir.join("firmware_rev"), "FW1\n").unwrap();
        let ns_dir = ctrl_dir.join("nvme0n1");
        std::fs::create_dir_all(&ns_dir).expect("create ns_dir");
        std::fs::write(ns_dir.join("size"), "1000\n").unwrap();

        // 1. IoCode(EACCES) -> PermissionDenied, code 13, smart None, health Unknown
        let dev_eacces = sys::read_controller_sysfs_with(&root, "nvme0", &|_| {
            Err(NvmeError::IoCode(libc::EACCES))
        })
        .expect("read_controller_sysfs_with");
        assert_eq!(dev_eacces.model, "FIXTURE SSD");
        assert_eq!(dev_eacces.serial, "SERIAL123");
        assert_eq!(dev_eacces.firmware, "FW1");
        assert_eq!(dev_eacces.total_bytes, 1000 * 512);
        assert!(dev_eacces.smart.is_none());
        assert_eq!(dev_eacces.telemetry.state, TelemetryReadState::Unavailable);
        assert_eq!(
            dev_eacces.telemetry.error,
            Some(TelemetryError {
                kind: TelemetryErrorKind::PermissionDenied,
                code: Some(libc::EACCES),
            })
        );
        assert_eq!(dev_eacces.health_state, HealthState::Unknown);

        // 2. IoCode(EIO) -> Io, code 5, smart None, health Unknown
        let dev_eio =
            sys::read_controller_sysfs_with(&root, "nvme0", &|_| Err(NvmeError::IoCode(libc::EIO)))
                .expect("read_controller_sysfs_with");
        assert!(dev_eio.smart.is_none());
        assert_eq!(dev_eio.telemetry.state, TelemetryReadState::Unavailable);
        assert_eq!(
            dev_eio.telemetry.error,
            Some(TelemetryError {
                kind: TelemetryErrorKind::Io,
                code: Some(libc::EIO),
            })
        );
        assert_eq!(dev_eio.health_state, HealthState::Unknown);

        // 3. CommandStatus(2) -> NvmeStatus, code 2, smart None, health Unknown
        let dev_cmd =
            sys::read_controller_sysfs_with(&root, "nvme0", &|_| Err(NvmeError::CommandStatus(2)))
                .expect("read_controller_sysfs_with");
        assert!(dev_cmd.smart.is_none());
        assert_eq!(dev_cmd.telemetry.state, TelemetryReadState::Unavailable);
        assert_eq!(
            dev_cmd.telemetry.error,
            Some(TelemetryError {
                kind: TelemetryErrorKind::NvmeStatus,
                code: Some(2),
            })
        );
        assert_eq!(dev_cmd.health_state, HealthState::Unknown);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nvme_admin_result_requires_zero() {
        assert_eq!(sys::check_admin_result(0, libc::EIO), Ok(()));
        assert_eq!(
            sys::check_admin_result(2, libc::EACCES),
            Err(NvmeError::CommandStatus(2))
        );
        assert_eq!(
            sys::check_admin_result(0x4002, 0),
            Err(NvmeError::CommandStatus(0x4002))
        );
        assert_eq!(
            sys::check_admin_result(-1, libc::EIO),
            Err(NvmeError::IoCode(libc::EIO))
        );
    }

    #[test]
    fn test_smart_log_parsing_standard() {
        let mut buf = [0u8; 512];

        // Byte 0: critical_warning bitmask
        // Set bit 0 (spare below threshold) and bit 1 (temperature exceeded)
        buf[0] = 0b0000_0011;

        // Bytes 1..=2: Composite temperature in Kelvin (LE u16)
        // 310 Kelvin = 36.85 Celsius -> 310 = 0x0136
        let temp_k: u16 = 310;
        buf[1..=2].copy_from_slice(&temp_k.to_le_bytes());

        // Byte 3: available spare (%)
        buf[3] = 95;

        // Byte 4: available spare threshold (%)
        buf[4] = 10;

        // Byte 5: percentage used (%)
        buf[5] = 5;

        // Bytes 32..48: data units read (128-bit LE)
        // 2000 units -> 2000 * 1000 * 512 = 1,024,000,000 bytes
        let units_read: u128 = 2000;
        buf[32..48].copy_from_slice(&units_read.to_le_bytes());

        // Bytes 48..64: data units written (128-bit LE)
        // 5000 units -> 5000 * 1000 * 512 = 2,560,000,000 bytes
        let units_written: u128 = 5000;
        buf[48..64].copy_from_slice(&units_written.to_le_bytes());

        // Bytes 64..80: host read commands (128-bit LE)
        let host_reads: u128 = 123_456;
        buf[64..80].copy_from_slice(&host_reads.to_le_bytes());

        // Bytes 80..96: host write commands (128-bit LE)
        let host_writes: u128 = 654_321;
        buf[80..96].copy_from_slice(&host_writes.to_le_bytes());

        // Bytes 128..144: power on hours (128-bit LE)
        let power_on_hours: u128 = 1200;
        buf[128..144].copy_from_slice(&power_on_hours.to_le_bytes());

        // Bytes 144..160: unsafe shutdowns (128-bit LE)
        let unsafe_shutdowns: u128 = 3;
        buf[144..160].copy_from_slice(&unsafe_shutdowns.to_le_bytes());

        // Bytes 160..176: media errors (128-bit LE)
        let media_errors: u128 = 0;
        buf[160..176].copy_from_slice(&media_errors.to_le_bytes());

        // Bytes 176..192: num error log entries (128-bit LE)
        let num_err_log_entries: u128 = 1;
        buf[176..192].copy_from_slice(&num_err_log_entries.to_le_bytes());

        let smart = parse_smart_log(&buf).expect("failed to parse smart log");

        assert_eq!(smart.critical_warning, 0b0000_0011);
        assert_eq!(
            smart.warning_flags,
            vec![
                "available_spare_below_threshold".to_string(),
                "temperature_exceeded".to_string()
            ]
        );
        // (310.0 - 273.15) = 36.85
        assert!((smart.temperature_c - 36.85).abs() < 0.001);
        assert_eq!(smart.available_spare_percent, 95);
        assert_eq!(smart.spare_threshold_percent, 10);
        assert_eq!(smart.percentage_used, 5);
        assert_eq!(smart.data_read_bytes, 1_024_000_000);
        assert_eq!(smart.data_written_bytes, 2_560_000_000);
        assert_eq!(smart.host_read_commands, 123_456);
        assert_eq!(smart.host_write_commands, 654_321);
        assert_eq!(smart.power_on_hours, 1200);
        assert_eq!(smart.unsafe_shutdowns, 3);
        assert_eq!(smart.media_errors, 0);
        assert_eq!(smart.num_err_log_entries, 1);
    }

    #[test]
    fn test_smart_log_warning_flags_all() {
        let mut buf = [0u8; 512];
        buf[0] = 0b0011_1111; // bits 0..5
        let smart = parse_smart_log(&buf).expect("failed to parse smart log");
        assert_eq!(smart.warning_flags.len(), 6);
        assert_eq!(
            smart.warning_flags,
            vec![
                "available_spare_below_threshold".to_string(),
                "temperature_exceeded".to_string(),
                "reliability_degraded".to_string(),
                "read_only".to_string(),
                "volatile_memory_backup_failed".to_string(),
                "persistent_memory_read_only".to_string(),
            ]
        );
    }

    #[test]
    fn test_smart_log_zero_temperature() {
        let mut buf = [0u8; 512];
        // Kelvin == 0
        buf[1..=2].copy_from_slice(&0u16.to_le_bytes());
        let smart = parse_smart_log(&buf).expect("failed to parse smart log");
        assert_eq!(smart.temperature_c, 0.0);
    }

    #[test]
    fn test_smart_log_overflow_saturation() {
        let mut buf = [0u8; 512];
        let huge: u128 = u128::MAX;
        buf[32..48].copy_from_slice(&huge.to_le_bytes());
        buf[64..80].copy_from_slice(&huge.to_le_bytes());
        let smart = parse_smart_log(&buf).expect("failed to parse smart log");
        assert_eq!(smart.data_read_bytes, u64::MAX);
        assert_eq!(smart.host_read_commands, u64::MAX);
    }

    #[test]
    fn test_nvme_probing_no_devices_nonexistent_sysfs() {
        let root = std::env::temp_dir().join(format!("rsetup-nvme-{}", uuid::Uuid::new_v4()));
        let non_existent = root.join("non_existent_sys");
        let manager = NvmeManager::probe_and_init(Some(&non_existent));
        assert!(!manager.is_initialized());
        let status = manager.status().expect("status");
        assert!(!status.initialized);
        assert!(status.devices.is_empty());
        assert_eq!(
            status.message,
            Some("No NVMe controller detected in system".into())
        );
    }

    #[test]
    fn test_nvme_probing_no_devices_empty_dir() {
        let root = std::env::temp_dir().join(format!("rsetup-nvme-{}", uuid::Uuid::new_v4()));
        let nvme_class = root.join("sys/class/nvme");
        std::fs::create_dir_all(&nvme_class).expect("create_dir_all");
        let manager = NvmeManager::probe_and_init(Some(&root));
        assert!(!manager.is_initialized());
        let status = manager.status().expect("status");
        assert!(!status.initialized);
        assert!(status.devices.is_empty());
        assert_eq!(
            status.message,
            Some("No NVMe controller detected in system".into())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_nvme_probing_with_devices() {
        let root = std::env::temp_dir().join(format!("rsetup-nvme-{}", uuid::Uuid::new_v4()));
        let nvme_class = root.join("sys/class/nvme");
        std::fs::create_dir_all(nvme_class.join("nvme0")).expect("create nvme0");
        std::fs::create_dir_all(nvme_class.join("nvme1")).expect("create nvme1");
        // Also add non-controller or namespace entries like nvme0n1 or other files to verify filtering
        std::fs::create_dir_all(nvme_class.join("nvme0n1")).expect("create nvme0n1");
        std::fs::write(nvme_class.join("other_file"), b"").expect("write other_file");

        let manager = NvmeManager::probe_and_init(Some(&root));
        assert!(manager.is_initialized());
        assert_eq!(manager.sysfs_root(), root.as_path());
        let status = manager.status().expect("status");
        assert!(status.initialized);
        assert_eq!(status.message, None);
        assert_eq!(status.devices.len(), 2);
        let names: Vec<String> = status.devices.into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["nvme0".to_string(), "nvme1".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_sysfs_controller_reading() {
        let root = std::env::temp_dir().join(format!("rsetup-nvme-sys-{}", uuid::Uuid::new_v4()));
        let ctrl_dir = root.join("sys/class/nvme/nvme0");
        std::fs::create_dir_all(&ctrl_dir).expect("create ctrl_dir");

        // Write sysfs attributes
        std::fs::write(ctrl_dir.join("model"), "Radxa NVMe SSD 256GB\n").unwrap();
        std::fs::write(ctrl_dir.join("serial"), "RADXA2026NVME01\n").unwrap();
        std::fs::write(ctrl_dir.join("firmware_rev"), "V1.00\n").unwrap();

        // Namespace nvme0n1 with size in 512-byte blocks: 500118192 * 512 = 256,060,514,304 bytes
        let ns_dir = ctrl_dir.join("nvme0n1");
        std::fs::create_dir_all(&ns_dir).expect("create ns_dir");
        std::fs::write(ns_dir.join("size"), "500118192\n").unwrap();

        let device = sys::read_controller_sysfs(&root, "nvme0").expect("read_controller_sysfs");
        assert_eq!(device.name, "nvme0");
        assert_eq!(device.path, "/dev/nvme0");
        assert_eq!(device.model, "Radxa NVMe SSD 256GB");
        assert_eq!(device.serial, "RADXA2026NVME01");
        assert_eq!(device.firmware, "V1.00");
        assert_eq!(device.total_bytes, 500118192 * 512);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_sysfs_controller_reading_with_injected_reader() {
        let root = std::env::temp_dir().join(format!("rsetup-nvme-sys-{}", uuid::Uuid::new_v4()));
        let ctrl_dir = root.join("sys/class/nvme/nvme0");
        std::fs::create_dir_all(&ctrl_dir).expect("create ctrl_dir");

        std::fs::write(ctrl_dir.join("model"), "Radxa NVMe SSD 256GB\n").unwrap();
        std::fs::write(ctrl_dir.join("serial"), "RADXA2026NVME01\n").unwrap();
        std::fs::write(ctrl_dir.join("firmware_rev"), "V1.00\n").unwrap();

        let ns_dir = ctrl_dir.join("nvme0n1");
        std::fs::create_dir_all(&ns_dir).expect("create ns_dir");
        std::fs::write(ns_dir.join("size"), "500118192\n").unwrap();

        // 1. Without injected reader on non-root, read_controller_sysfs delegates to default fixture reader
        // which rejects reading with NotSupported("fixture requires an injected reader")
        // and sets smart to None and telemetry to Unavailable/Io/None.
        let dev_default =
            sys::read_controller_sysfs(&root, "nvme0").expect("read_controller_sysfs");
        assert!(dev_default.smart.is_none());
        assert_eq!(dev_default.telemetry.state, TelemetryReadState::Unavailable);
        assert_eq!(
            dev_default.telemetry.error,
            Some(TelemetryError {
                kind: TelemetryErrorKind::Io,
                code: None,
            })
        );
        assert_eq!(dev_default.health_state, HealthState::Unknown);

        // 2. With injected reader and call count verification
        let call_count = std::sync::atomic::AtomicUsize::new(0);
        let mut fake_buf = [0u8; 512];
        let temp_k: u16 = 310;
        fake_buf[1..=2].copy_from_slice(&temp_k.to_le_bytes());

        let reader = |dev_path: &str| {
            assert_eq!(dev_path, "/dev/nvme0");
            call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(fake_buf)
        };

        let device = sys::read_controller_sysfs_with(&root, "nvme0", &reader)
            .expect("read_controller_sysfs_with");

        assert_eq!(device.name, "nvme0");
        assert_eq!(device.path, "/dev/nvme0");
        assert_eq!(device.model, "Radxa NVMe SSD 256GB");
        assert_eq!(device.serial, "RADXA2026NVME01");
        assert_eq!(device.firmware, "V1.00");
        assert_eq!(device.total_bytes, 500118192 * 512);

        // kelvin = 310 -> 310 - 273.15 = 36.85°C
        assert!(device.smart.is_some());
        assert!((device.smart.as_ref().unwrap().temperature_c - 36.85).abs() < 0.001);
        assert_eq!(device.telemetry.state, TelemetryReadState::Available);
        assert_eq!(device.health_state, HealthState::Healthy);
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_sys_smart_log_graceful_error_on_nonexistent_device() {
        let res = sys::read_smart_log_raw("/dev/nonexistent_nvme_device_xyz");
        assert!(res.is_err());
        match res.unwrap_err() {
            NvmeError::IoCode(code) => {
                assert_eq!(code, libc::ENOENT);
            }
            other => panic!("expected NvmeError::IoCode, got {:?}", other),
        }
    }

    #[test]
    fn storage_freshness_nvme_rereads_smart() {
        let root =
            std::env::temp_dir().join(format!("rsetup-nvme-freshness-{}", uuid::Uuid::new_v4()));
        let ctrl_dir = root.join("sys/class/nvme/nvme0");
        std::fs::create_dir_all(&ctrl_dir).expect("create ctrl_dir");
        std::fs::write(ctrl_dir.join("model"), "TEST NVME\n").unwrap();

        let manager = NvmeManager::probe_and_init(Some(&root));

        use std::cell::Cell;
        let call_count = Cell::new(0usize);
        let reader = |_dev: &str| -> Result<[u8; 512], NvmeError> {
            let count = call_count.get();
            call_count.set(count + 1);
            let mut buf = [0u8; 512];
            // Kelvin: 310 then 320 at bytes 1..3 LE
            let kelvin: u16 = if count == 0 { 310 } else { 320 };
            buf[1..3].copy_from_slice(&kelvin.to_le_bytes());
            Ok(buf)
        };

        let status1 = manager.status_with(&reader).expect("status 1");
        assert_eq!(call_count.get(), 1);
        let temp1 = status1.devices[0].smart.as_ref().unwrap().temperature_c;
        assert!((temp1 - (310.0 - 273.15)).abs() < 0.01);

        let status2 = manager.status_with(&reader).expect("status 2");
        assert_eq!(call_count.get(), 2);
        let temp2 = status2.devices[0].smart.as_ref().unwrap().temperature_c;
        assert!((temp2 - (320.0 - 273.15)).abs() < 0.01);
        assert!((temp1 - temp2).abs() > 1.0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nvme_enumeration_distinguishes_missing_empty_and_errors() {
        let temp = std::env::temp_dir();

        // 1. Missing directory => empty success
        let missing = temp.join(format!("rsetup-nvme-missing-{}", uuid::Uuid::new_v4()));
        let res =
            NvmeManager::try_probe_sysfs(&missing).expect("missing directory is empty success");
        assert!(res.is_empty());

        // 2. Empty directory => empty success
        let empty_root = temp.join(format!("rsetup-nvme-empty-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(empty_root.join("sys/class/nvme")).unwrap();
        let res = NvmeManager::try_probe_sysfs(&empty_root).expect("empty dir is empty success");
        assert!(res.is_empty());
        let _ = std::fs::remove_dir_all(&empty_root);

        // 3. Normal file occupying subsystem directory => ENOTDIR error
        let file_root = temp.join(format!("rsetup-nvme-notdir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(file_root.join("sys/class")).unwrap();
        std::fs::write(file_root.join("sys/class/nvme"), "not a directory").unwrap();
        let err = NvmeManager::try_probe_sysfs(&file_root).unwrap_err();
        match err {
            NvmeError::IoCode(code) => assert_eq!(code, libc::ENOTDIR),
            other => panic!("expected ENOTDIR IoCode, got {:?}", other),
        }
        let _ = std::fs::remove_dir_all(&file_root);
    }

    #[test]
    fn nvme_same_manager_add_and_remove_controllers() {
        let root =
            std::env::temp_dir().join(format!("rsetup-nvme-addrem-{}", uuid::Uuid::new_v4()));
        let nvme_dir = root.join("sys/class/nvme");
        std::fs::create_dir_all(&nvme_dir).unwrap();

        let manager = NvmeManager::probe_and_init(Some(&root));
        let s0 = manager.status().unwrap();
        assert_eq!(s0.devices.len(), 0);

        // Add nvme0
        let nvme0 = nvme_dir.join("nvme0");
        std::fs::create_dir_all(&nvme0).unwrap();
        std::fs::write(nvme0.join("model"), "NVME 0").unwrap();

        let s1 = manager.status().unwrap();
        assert_eq!(s1.devices.len(), 1);
        assert_eq!(s1.devices[0].name, "nvme0");

        // Add nvme1
        let nvme1 = nvme_dir.join("nvme1");
        std::fs::create_dir_all(&nvme1).unwrap();
        std::fs::write(nvme1.join("model"), "NVME 1").unwrap();

        let s2 = manager.status().unwrap();
        assert_eq!(s2.devices.len(), 2);

        // Remove nvme0
        std::fs::remove_dir_all(&nvme0).unwrap();
        let s3 = manager.status().unwrap();
        assert_eq!(s3.devices.len(), 1);
        assert_eq!(s3.devices[0].name, "nvme1");

        let _ = std::fs::remove_dir_all(&root);
    }
}
