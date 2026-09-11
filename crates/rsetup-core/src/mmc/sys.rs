use super::{parse_life_time_str, parse_pre_eol_info_str, format_manufacturer, generate_warning_flags, MmcError};
use crate::model::{MmcDevice, MmcHealth};
use std::fs;
use std::path::{Path, PathBuf};

/// Read a trimmed string from a file if it exists.
pub fn read_trimmed_attr(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Read block device size in bytes from sysfs.
pub fn read_block_device_size(sysfs_root: &Path, block_name: &str) -> u64 {
    let size_file = if sysfs_root == Path::new("/") {
        PathBuf::from(format!("/sys/class/block/{}/size", block_name))
    } else {
        sysfs_root.join(format!("sys/class/block/{}/size", block_name))
    };

    if let Ok(size_str) = fs::read_to_string(&size_file) {
        if let Ok(blocks) = size_str.trim().parse::<u64>() {
            return blocks.saturating_mul(512);
        }
    }
    0
}

/// Read sysfs MMC/SD device info and construct an MmcDevice.
pub fn read_device_sysfs(sysfs_root: &Path, dev_name: &str) -> Result<MmcDevice, MmcError> {
    let dev_dir = if sysfs_root == Path::new("/") {
        PathBuf::from(format!("/sys/bus/mmc/devices/{}", dev_name))
    } else {
        sysfs_root.join(format!("sys/bus/mmc/devices/{}", dev_name))
    };

    if !dev_dir.exists() {
        return Err(MmcError::Io(format!(
            "MMC device directory does not exist: {}",
            dev_dir.display()
        )));
    }

    let card_type = read_trimmed_attr(&dev_dir.join("type")).unwrap_or_default();
    if card_type == "SDIO" {
        return Err(MmcError::NotSupported("SDIO device ignored".to_string()));
    }

    let model = read_trimmed_attr(&dev_dir.join("name")).unwrap_or_default();
    let manfid_raw = read_trimmed_attr(&dev_dir.join("manfid")).unwrap_or_default();
    let manufacturer = format_manufacturer(&manfid_raw);
    let serial = read_trimmed_attr(&dev_dir.join("serial")).unwrap_or_default();

    let firmware = read_trimmed_attr(&dev_dir.join("fwrev"))
        .or_else(|| read_trimmed_attr(&dev_dir.join("prv")))
        .or_else(|| read_trimmed_attr(&dev_dir.join("hwrev")))
        .unwrap_or_default();

    let life_time_str = read_trimmed_attr(&dev_dir.join("life_time")).unwrap_or_default();
    let (life_a, life_b) = parse_life_time_str(&life_time_str);

    let pre_eol_str = read_trimmed_attr(&dev_dir.join("pre_eol_info")).unwrap_or_default();
    let pre_eol = parse_pre_eol_info_str(&pre_eol_str);

    let warning_flags = generate_warning_flags(pre_eol, life_a, life_b);

    let health = MmcHealth {
        pre_eol_info: pre_eol,
        life_time_est_a_percent: life_a,
        life_time_est_b_percent: life_b,
        warning_flags,
    };

    // Determine block device
    // Check dev_dir/block or entries matching block/mmcblk*
    let mut block_name: Option<String> = None;

    let block_dir = dev_dir.join("block");
    if block_dir.exists() {
        if let Ok(entries) = fs::read_dir(&block_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("mmcblk") {
                    block_name = Some(name);
                    break;
                }
            }
        }
    }

    // Fallback: check dev_dir entries directly for mmcblk*
    if block_name.is_none() {
        if let Ok(entries) = fs::read_dir(&dev_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("mmcblk") {
                    block_name = Some(name);
                    break;
                }
            }
        }
    }

    let (block_path, total_bytes) = if let Some(blk) = block_name {
        let size = read_block_device_size(sysfs_root, &blk);
        (format!("/dev/{}", blk), size)
    } else {
        (String::new(), 0)
    };

    Ok(MmcDevice {
        name: dev_name.to_string(),
        block_path,
        card_type,
        model,
        manufacturer,
        serial,
        firmware,
        total_bytes,
        health,
    })
}
