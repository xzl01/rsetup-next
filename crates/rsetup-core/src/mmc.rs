use crate::model::MmcStatus;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub mod sys;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MmcError {
    #[error("Invalid buffer length: expected {expected}, got {actual}")]
    InvalidBufferLength { expected: usize, actual: usize },
    #[error("Device not supported: {0}")]
    NotSupported(String),
    #[error("I/O error: {0}")]
    Io(String),
}

/// MMC/SD device manager.
#[derive(Debug, Clone)]
pub struct MmcManager {
    status: MmcStatus,
    sysfs_root: PathBuf,
}

impl Default for MmcManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MmcManager {
    /// Probe the system for MMC/SD devices and initialize manager status.
    pub fn probe_and_init(sysfs_root: Option<&Path>) -> Self {
        let root = sysfs_root
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        let devices = Self::probe_sysfs(&root);

        let status = if devices.is_empty() {
            MmcStatus {
                initialized: false,
                devices: Vec::new(),
                message: Some("No MMC/SD devices detected in system".into()),
            }
        } else {
            let mmc_devices = devices
                .into_iter()
                .filter_map(|name| sys::read_device_sysfs(&root, &name).ok())
                .collect();
            MmcStatus {
                initialized: true,
                devices: mmc_devices,
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

    pub fn status(&self) -> MmcStatus {
        self.status.clone()
    }

    pub fn is_initialized(&self) -> bool {
        self.status.initialized
    }

    /// Probe `sys/bus/mmc/devices` under the given root.
    pub fn probe_sysfs(root: &Path) -> Vec<String> {
        let mmc_bus_dir = if root == Path::new("/") {
            PathBuf::from("/sys/bus/mmc/devices")
        } else {
            root.join("sys/bus/mmc/devices")
        };

        let mut devices = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&mmc_bus_dir) {
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy().to_string();
                let dev_dir = entry.path();
                let type_file = dev_dir.join("type");
                if let Some(card_type) = sys::read_trimmed_attr(&type_file) {
                    if card_type == "MMC" || card_type == "SD" {
                        devices.push(name);
                    }
                }
            }
        }
        devices.sort();
        devices
    }
}

fn parse_hex_or_dec_u8(s: &str) -> Option<u8> {
    let s = s.trim();
    if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u8::from_str_radix(hex_str, 16).ok()
    } else {
        s.parse::<u8>().ok()
    }
}

fn map_life_time_val(val: u8) -> Option<u8> {
    match val {
        0x00 => None,
        0x01..=0x0A => Some(val * 10),
        0x0B => Some(101),
        _ => None,
    }
}

pub fn parse_life_time_str(s: &str) -> (Option<u8>, Option<u8>) {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.len() != 2 {
        return (None, None);
    }

    let val_a = parse_hex_or_dec_u8(tokens[0]).and_then(map_life_time_val);
    let val_b = parse_hex_or_dec_u8(tokens[1]).and_then(map_life_time_val);

    (val_a, val_b)
}

pub fn parse_pre_eol_info_str(s: &str) -> u8 {
    let val = parse_hex_or_dec_u8(s);
    match val {
        Some(1) => 1,
        Some(2) => 2,
        Some(3) => 3,
        _ => 0,
    }
}

pub fn parse_manfid_to_name(manfid: u32) -> &'static str {
    match manfid {
        0x15 => "Samsung",
        0x90 => "SK Hynix",
        0x13 => "Micron",
        0x45 => "SanDisk",
        0x70 => "Kingston",
        0x11 => "Toshiba",
        0xfe => "Micron",
        _ => "Unknown",
    }
}

pub fn format_manufacturer(manfid_str: &str) -> String {
    let s = manfid_str.trim();
    let parsed_val = if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex_str, 16).ok()
    } else {
        s.parse::<u32>().ok()
    };

    if let Some(val) = parsed_val {
        let name = parse_manfid_to_name(val);
        if name != "Unknown" {
            format!("{} ({})", name, s)
        } else {
            s.to_string()
        }
    } else {
        s.to_string()
    }
}

pub fn generate_warning_flags(
    pre_eol: u8,
    life_a: Option<u8>,
    life_b: Option<u8>,
) -> Vec<String> {
    let mut flags = Vec::new();
    if pre_eol == 2 {
        flags.push("pre_eol_warning".to_string());
    } else if pre_eol == 3 {
        flags.push("pre_eol_urgent".to_string());
    }

    if life_a.map(|v| v >= 100).unwrap_or(false) {
        flags.push("life_time_typ_a_exceeded".to_string());
    }
    if life_b.map(|v| v >= 100).unwrap_or(false) {
        flags.push("life_time_typ_b_exceeded".to_string());
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_life_time() {
        // Valid pairs
        assert_eq!(parse_life_time_str("0x01 0x02"), (Some(10), Some(20)));
        assert_eq!(parse_life_time_str("0x01\n0x02"), (Some(10), Some(20)));
        assert_eq!(parse_life_time_str("1 2"), (Some(10), Some(20)));
        assert_eq!(parse_life_time_str("0x0A 0x05"), (Some(100), Some(50)));

        // Exceeded
        assert_eq!(parse_life_time_str("0x0B 0x0B"), (Some(101), Some(101)));

        // Invalid values / 0x00 (Not defined) / 0x0C (Reserved)
        assert_eq!(parse_life_time_str("0x00 0x0C"), (None, None));
        assert_eq!(parse_life_time_str("0x01 0x00"), (Some(10), None));
        assert_eq!(parse_life_time_str("0x00 0x02"), (None, Some(20)));

        // Empty or invalid format
        assert_eq!(parse_life_time_str(""), (None, None));
        assert_eq!(parse_life_time_str("not_a_number"), (None, None));
        assert_eq!(parse_life_time_str("0x01"), (None, None));
    }

    #[test]
    fn test_parse_pre_eol_info() {
        assert_eq!(parse_pre_eol_info_str("0x01"), 1);
        assert_eq!(parse_pre_eol_info_str("1"), 1);
        assert_eq!(parse_pre_eol_info_str("0x02"), 2);
        assert_eq!(parse_pre_eol_info_str("2"), 2);
        assert_eq!(parse_pre_eol_info_str("0x03"), 3);
        assert_eq!(parse_pre_eol_info_str("3"), 3);

        // Invalid / unknown
        assert_eq!(parse_pre_eol_info_str("0x00"), 0);
        assert_eq!(parse_pre_eol_info_str("0x04"), 0);
        assert_eq!(parse_pre_eol_info_str(""), 0);
        assert_eq!(parse_pre_eol_info_str("foo"), 0);
    }

    #[test]
    fn test_format_manufacturer() {
        assert_eq!(format_manufacturer("0x000015"), "Samsung (0x000015)");
        assert_eq!(format_manufacturer("0x15"), "Samsung (0x15)");
        assert_eq!(format_manufacturer("0x000090"), "SK Hynix (0x000090)");
        assert_eq!(format_manufacturer("0x000013"), "Micron (0x000013)");
        assert_eq!(format_manufacturer("0x000045"), "SanDisk (0x000045)");
        assert_eq!(format_manufacturer("0x000070"), "Kingston (0x000070)");
        assert_eq!(format_manufacturer("0x000011"), "Toshiba (0x000011)");
        assert_eq!(format_manufacturer("0x0000fe"), "Micron (0x0000fe)");
        assert_eq!(format_manufacturer("0x000099"), "0x000099");
        assert_eq!(format_manufacturer("invalid"), "invalid");
    }

    #[test]
    fn test_generate_warning_flags() {
        // Normal
        assert_eq!(
            generate_warning_flags(1, Some(50), Some(60)),
            Vec::<String>::new()
        );

        // Pre EOL warning
        assert_eq!(
            generate_warning_flags(2, Some(50), Some(60)),
            vec!["pre_eol_warning".to_string()]
        );

        // Pre EOL urgent
        assert_eq!(
            generate_warning_flags(3, Some(50), Some(60)),
            vec!["pre_eol_urgent".to_string()]
        );

        // Typ A exceeded
        assert_eq!(
            generate_warning_flags(1, Some(100), Some(60)),
            vec!["life_time_typ_a_exceeded".to_string()]
        );
        assert_eq!(
            generate_warning_flags(1, Some(101), Some(60)),
            vec!["life_time_typ_a_exceeded".to_string()]
        );

        // Typ B exceeded
        assert_eq!(
            generate_warning_flags(1, Some(50), Some(100)),
            vec!["life_time_typ_b_exceeded".to_string()]
        );

        // All flags
        assert_eq!(
            generate_warning_flags(3, Some(100), Some(101)),
            vec![
                "pre_eol_urgent".to_string(),
                "life_time_typ_a_exceeded".to_string(),
                "life_time_typ_b_exceeded".to_string()
            ]
        );
    }

    #[test]
    fn test_mmc_probing_no_devices() {
        let root = std::env::temp_dir().join(format!("rsetup-mmc-none-{}", uuid::Uuid::new_v4()));
        let manager = MmcManager::probe_and_init(Some(&root));
        assert!(!manager.is_initialized());
        let status = manager.status();
        assert!(!status.initialized);
        assert_eq!(status.devices.len(), 0);
        assert_eq!(
            status.message,
            Some("No MMC/SD devices detected in system".into())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_mmc_probing_multi_devices() {
        let root = std::env::temp_dir().join(format!("rsetup-mmc-multi-{}", uuid::Uuid::new_v4()));

        // mmc0:0001: MMC
        let dev0_dir = root.join("sys/bus/mmc/devices/mmc0:0001");
        std::fs::create_dir_all(&dev0_dir).expect("create dev0_dir");
        std::fs::write(dev0_dir.join("type"), "MMC\n").unwrap();
        std::fs::write(dev0_dir.join("name"), "FE4MB4\n").unwrap();
        std::fs::write(dev0_dir.join("manfid"), "0x000015\n").unwrap();
        std::fs::write(dev0_dir.join("serial"), "0x12345678\n").unwrap();
        std::fs::write(dev0_dir.join("life_time"), "0x01 0x01\n").unwrap();
        std::fs::write(dev0_dir.join("pre_eol_info"), "0x01\n").unwrap();
        let blk0_dir = dev0_dir.join("block/mmcblk0");
        std::fs::create_dir_all(&blk0_dir).expect("create blk0_dir");
        let class_blk0 = root.join("sys/class/block/mmcblk0");
        std::fs::create_dir_all(&class_blk0).expect("create class_blk0");
        std::fs::write(class_blk0.join("size"), "122142720\n").unwrap();

        // mmc1:59b4: SD
        let dev1_dir = root.join("sys/bus/mmc/devices/mmc1:59b4");
        std::fs::create_dir_all(&dev1_dir).expect("create dev1_dir");
        std::fs::write(dev1_dir.join("type"), "SD\n").unwrap();
        std::fs::write(dev1_dir.join("name"), "SC64G\n").unwrap();
        std::fs::write(dev1_dir.join("manfid"), "0x000045\n").unwrap();
        std::fs::write(dev1_dir.join("serial"), "0x87654321\n").unwrap();
        let blk1_dir = dev1_dir.join("block/mmcblk1");
        std::fs::create_dir_all(&blk1_dir).expect("create blk1_dir");
        let class_blk1 = root.join("sys/class/block/mmcblk1");
        std::fs::create_dir_all(&class_blk1).expect("create class_blk1");
        std::fs::write(class_blk1.join("size"), "124735488\n").unwrap();

        // mmc2:0001: SDIO (should be ignored)
        let dev2_dir = root.join("sys/bus/mmc/devices/mmc2:0001");
        std::fs::create_dir_all(&dev2_dir).expect("create dev2_dir");
        std::fs::write(dev2_dir.join("type"), "SDIO\n").unwrap();
        std::fs::write(dev2_dir.join("name"), "WIFI\n").unwrap();

        let manager = MmcManager::probe_and_init(Some(&root));
        assert!(manager.is_initialized());
        assert_eq!(manager.sysfs_root(), root.as_path());
        let status = manager.status();
        assert!(status.initialized);
        assert_eq!(status.message, None);
        assert_eq!(status.devices.len(), 2);

        let d0 = &status.devices[0];
        assert_eq!(d0.name, "mmc0:0001");
        assert_eq!(d0.card_type, "MMC");
        assert_eq!(d0.model, "FE4MB4");
        assert_eq!(d0.manufacturer, "Samsung (0x000015)");
        assert_eq!(d0.serial, "0x12345678");
        assert_eq!(d0.block_path, "/dev/mmcblk0");
        assert_eq!(d0.total_bytes, 62537072640);
        assert_eq!(d0.health.pre_eol_info, 1);
        assert_eq!(d0.health.life_time_est_a_percent, Some(10));
        assert_eq!(d0.health.life_time_est_b_percent, Some(10));
        assert!(d0.health.warning_flags.is_empty());

        let d1 = &status.devices[1];
        assert_eq!(d1.name, "mmc1:59b4");
        assert_eq!(d1.card_type, "SD");
        assert_eq!(d1.model, "SC64G");
        assert_eq!(d1.manufacturer, "SanDisk (0x000045)");
        assert_eq!(d1.serial, "0x87654321");
        assert_eq!(d1.block_path, "/dev/mmcblk1");
        assert_eq!(d1.total_bytes, 124735488 * 512);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_mmc_primary_block_device_selection() {
        let root = std::env::temp_dir().join(format!("rsetup-mmc-blk-filter-{}", uuid::Uuid::new_v4()));
        let dev_dir = root.join("sys/bus/mmc/devices/mmc0:0001");
        std::fs::create_dir_all(&dev_dir).expect("create dev_dir");
        std::fs::write(dev_dir.join("type"), "MMC\n").unwrap();
        std::fs::write(dev_dir.join("name"), "TEST_MMC\n").unwrap();

        let block_dir = dev_dir.join("block");
        // Create boot0 as the ONLY file first to see if it mistakenly matches
        std::fs::create_dir_all(block_dir.join("mmcblk0boot0")).unwrap();

        let class_blk0boot0 = root.join("sys/class/block/mmcblk0boot0");
        std::fs::create_dir_all(&class_blk0boot0).unwrap();
        std::fs::write(class_blk0boot0.join("size"), "10\n").unwrap();

        // If only boot0 is present, it shouldn't match mmcblk0
        let dev = sys::read_device_sysfs(&root, "mmc0:0001").expect("read_device_sysfs");
        assert_eq!(dev.block_path, "");

        // Now add boot0, boot1, rpmb, p1, and mmcblk0
        std::fs::create_dir_all(block_dir.join("mmcblk0boot1")).unwrap();
        std::fs::create_dir_all(block_dir.join("mmcblk0rpmb")).unwrap();
        std::fs::create_dir_all(block_dir.join("mmcblk0p1")).unwrap();
        std::fs::create_dir_all(block_dir.join("mmcblk0")).unwrap();

        let class_blk0 = root.join("sys/class/block/mmcblk0");
        std::fs::create_dir_all(&class_blk0).unwrap();
        std::fs::write(class_blk0.join("size"), "1000\n").unwrap();

        let dev2 = sys::read_device_sysfs(&root, "mmc0:0001").expect("read_device_sysfs");
        assert_eq!(dev2.block_path, "/dev/mmcblk0");
        assert_eq!(dev2.total_bytes, 1000 * 512);

        let _ = std::fs::remove_dir_all(&root);
    }
}
