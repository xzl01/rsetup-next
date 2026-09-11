use crate::model::{NvmeDevice, NvmeSmartLog, NvmeStatus};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub mod sys;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NvmeError {
    #[error("Invalid SMART buffer length: expected {expected}, got {actual}")]
    InvalidBufferLength { expected: usize, actual: usize },
    #[error("Device not supported: {0}")]
    NotSupported(String),
    #[error("I/O error: {0}")]
    Io(String),
}

/// NVMe device and controller manager.
#[derive(Debug, Clone)]
pub struct NvmeManager {
    status: NvmeStatus,
    sysfs_root: PathBuf,
}

impl Default for NvmeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl NvmeManager {
    /// Probe the system for NVMe controllers and initialize manager status.
    pub fn probe_and_init(sysfs_root: Option<&Path>) -> Self {
        let root = sysfs_root
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        let devices = Self::probe_sysfs(&root);

        let status = if devices.is_empty() {
            NvmeStatus {
                initialized: false,
                devices: Vec::new(),
                message: Some("No NVMe controller detected in system".into()),
            }
        } else {
            let nvme_devices = devices
                .into_iter()
                .map(|name| {
                    sys::read_controller_sysfs(&root, &name).unwrap_or_else(|_| NvmeDevice {
                        path: format!("/dev/{}", name),
                        name,
                        model: String::new(),
                        serial: String::new(),
                        firmware: String::new(),
                        total_bytes: 0,
                        smart: NvmeSmartLog::default(),
                    })
                })
                .collect();
            NvmeStatus {
                initialized: true,
                devices: nvme_devices,
                message: None,
            }
        };

        Self {
            status,
            sysfs_root: root,
        }
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
        let nvme_class_dir = if root == Path::new("/") {
            PathBuf::from("/sys/class/nvme")
        } else {
            root.join("sys/class/nvme")
        };

        let mut devices = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&nvme_class_dir) {
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                // Match controller entries like nvme0, nvme1 (exclude namespace or other files)
                // Controllers are typically nvmeX where X is numeric.
                if let Some(suffix) = name.strip_prefix("nvme") {
                    if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                        devices.push(name.to_string());
                    }
                }
            }
        }
        devices.sort();
        devices
    }

    pub fn status(&self) -> NvmeStatus {
        self.status.clone()
    }

    pub fn is_initialized(&self) -> bool {
        self.status.initialized
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
        let status = manager.status();
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
        let status = manager.status();
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
        let status = manager.status();
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
    fn test_sys_smart_log_graceful_error_on_nonexistent_device() {
        let res = sys::read_smart_log_raw("/dev/nonexistent_nvme_device_xyz");
        assert!(res.is_err());
        match res.unwrap_err() {
            NvmeError::Io(msg) => {
                assert!(!msg.is_empty());
            }
            other => panic!("expected NvmeError::Io, got {:?}", other),
        }
    }
}
