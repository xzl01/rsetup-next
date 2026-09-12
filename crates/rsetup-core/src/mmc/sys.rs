use super::{
    MmcError, format_manufacturer, generate_warning_flags, map_life_time_byte_to_percent,
    parse_life_time_str, parse_pre_eol_info_str,
};
use crate::model::{MmcDevice, MmcHealth};
use std::fs;
use std::path::{Path, PathBuf};

/// Mirror of the kernel's `struct mmc_ioc_cmd`
/// (`include/uapi/linux/mmc/ioctl.h`), field-for-field. The kernel requires
/// this struct to be exactly 72 bytes with `data_ptr` 8-byte aligned, the
/// same on 32- and 64-bit, which `#[repr(C)]` with the trailing `u64` gives
/// us here.
#[repr(C)]
struct MmcIocCmd {
    write_flag: libc::c_int,
    is_acmd: libc::c_int,
    opcode: u32,
    arg: u32,
    response: [u32; 4],
    flags: u32,
    blksz: u32,
    blocks: u32,
    postsleep_min_us: u32,
    postsleep_max_us: u32,
    data_timeout_ns: u32,
    cmd_timeout_ms: u32,
    _pad: u32,
    data_ptr: u64,
}

/// `MMC_BLOCK_MAJOR` from `include/uapi/linux/major.h`.
const MMC_BLOCK_MAJOR: u8 = 179;

// _IOWR(MMC_BLOCK_MAJOR, 0, struct mmc_ioc_cmd)
// dir = _IOC_READ | _IOC_WRITE = 3, size = 72 (0x48), type = 179 (0xB3), nr = 0
// 3 << 30 | 72 << 16 | 0xB3 << 8 | 0x00 = 0xc048b300
const MMC_IOC_CMD: libc::c_ulong = 3u64 << 30 | 72u64 << 16 | (MMC_BLOCK_MAJOR as u64) << 8;

/// CMD8 — SEND EXT_CSD (include/linux/mmc/core.h).
const MMC_OPCODE_SEND_EXT_CSD: u32 = 8;

/// R1 response type: (1<<0) | (1<<2) | (1<<4).
const MMC_RSP_R1: u32 = 0x15;

/// Command carries an associated data transfer (include/linux/mmc/core.h).
const MMC_CMD_ADTC: u32 = 1 << 5;

// EXT_CSD byte offsets (include/linux/mmc/mmc.h)
const EXT_CSD_REV: usize = 192;
const EXT_CSD_SEC_CNT: usize = 212;
const EXT_CSD_FIRMWARE_VERSION: usize = 254;
const EXT_CSD_PRE_EOL_INFO: usize = 267;
const EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_A: usize = 268;
const EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_B: usize = 269;

/// Parsed subset of the 512-byte EXT_CSD register (CMD8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmcExtCsd {
    pub rev: u8,
    /// Capacity in 512-byte sectors (EXT_CSD[212..216], little-endian).
    pub sec_count: u32,
    /// ASCII firmware revision, NUL-padded (EXT_CSD[254..262]).
    pub firmware_version: [u8; 8],
    pub pre_eol_info: u8,
    /// Raw byte value: 0x00 undefined .. 0x0B exceeded.
    pub life_time_est_typ_a: u8,
    pub life_time_est_typ_b: u8,
}

/// Parse a 512-byte EXT_CSD buffer. Pure safe code — no device access.
pub fn parse_ext_csd(buf: &[u8; 512]) -> MmcExtCsd {
    let mut firmware_version = [0u8; 8];
    firmware_version.copy_from_slice(&buf[EXT_CSD_FIRMWARE_VERSION..EXT_CSD_FIRMWARE_VERSION + 8]);

    MmcExtCsd {
        rev: buf[EXT_CSD_REV],
        sec_count: u32::from_le_bytes([
            buf[EXT_CSD_SEC_CNT],
            buf[EXT_CSD_SEC_CNT + 1],
            buf[EXT_CSD_SEC_CNT + 2],
            buf[EXT_CSD_SEC_CNT + 3],
        ]),
        firmware_version,
        pre_eol_info: buf[EXT_CSD_PRE_EOL_INFO],
        life_time_est_typ_a: buf[EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_A],
        life_time_est_typ_b: buf[EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_B],
    }
}

/// Read the full 512-byte EXT_CSD register from an MMC block device node
/// (e.g. `/dev/mmcblk0`) via the direct `MMC_IOC_CMD` ioctl. Read-only;
/// same style as `nvme/sys.rs::read_smart_log_raw`.
pub fn read_ext_csd_raw(dev_path: &str) -> Result<[u8; 512], MmcError> {
    use std::ffi::CString;

    let c_path = CString::new(dev_path)
        .map_err(|e| MmcError::Io(format!("Invalid device path {}: {}", dev_path, e)))?;

    // Open read-only
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        return Err(MmcError::Io(format!(
            "Failed to open {}: {}",
            dev_path, err
        )));
    }

    let mut buf = [0u8; 512];

    let mut cmd = MmcIocCmd {
        write_flag: 0, // read
        is_acmd: 0,
        opcode: MMC_OPCODE_SEND_EXT_CSD,
        arg: 0, // EXT_CSD read starts at offset 0
        response: [0; 4],
        flags: MMC_RSP_R1 | MMC_CMD_ADTC,
        blksz: 512,
        blocks: 1,
        postsleep_min_us: 0,
        postsleep_max_us: 0,
        data_timeout_ns: 0,
        cmd_timeout_ms: 0,
        _pad: 0,
        data_ptr: buf.as_mut_ptr() as u64,
    };

    let ret = unsafe { libc::ioctl(fd, MMC_IOC_CMD, &mut cmd) };
    let ioctl_err = if ret < 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };

    unsafe { libc::close(fd) };

    if let Some(err) = ioctl_err {
        return Err(MmcError::Io(format!(
            "MMC_IOC_CMD failed on {}: {}",
            dev_path, err
        )));
    }

    Ok(buf)
}

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

/// Check if a block device name is a primary mmcblk device (e.g. "mmcblk0", "mmcblk1"),
/// excluding partitions ("mmcblk0p1"), boot partitions ("mmcblk0boot0"), rpmb ("mmcblk0rpmb"), etc.
pub fn is_primary_mmcblk(name: &str) -> bool {
    if let Some(suffix) = name.strip_prefix("mmcblk") {
        !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
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

    let mut firmware = read_trimmed_attr(&dev_dir.join("fwrev"))
        .or_else(|| read_trimmed_attr(&dev_dir.join("prv")))
        .or_else(|| read_trimmed_attr(&dev_dir.join("hwrev")))
        .unwrap_or_default();

    let life_time_str = read_trimmed_attr(&dev_dir.join("life_time")).unwrap_or_default();
    let (mut life_a, mut life_b) = parse_life_time_str(&life_time_str);

    let pre_eol_str = read_trimmed_attr(&dev_dir.join("pre_eol_info")).unwrap_or_default();
    let mut pre_eol = parse_pre_eol_info_str(&pre_eol_str);

    // Determine block device
    // Check dev_dir/block or entries matching block/mmcblk*
    let mut block_name: Option<String> = None;

    let block_dir = dev_dir.join("block");
    if block_dir.exists() {
        if let Ok(entries) = fs::read_dir(&block_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if is_primary_mmcblk(&name) {
                    block_name = Some(name);
                    break;
                }
            }
        }
    }

    // Fallback: check dev_dir entries directly for primary mmcblk*
    if block_name.is_none() {
        if let Ok(entries) = fs::read_dir(&dev_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if is_primary_mmcblk(&name) {
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

    // Fallback: when sysfs exposes no life time / pre-EOL info at all, try
    // reading EXT_CSD directly from the block device via MMC_IOC_CMD. On
    // failure, silently keep the sysfs results (unwrap_or_default semantics)
    // — no error logging, device probing is unaffected.
    if life_a.is_none() && life_b.is_none() && pre_eol == 0 && !block_path.is_empty() {
        if let Ok(buf) = read_ext_csd_raw(&block_path) {
            let ext = parse_ext_csd(&buf);
            pre_eol = ext.pre_eol_info;
            life_a = map_life_time_byte_to_percent(ext.life_time_est_typ_a);
            life_b = map_life_time_byte_to_percent(ext.life_time_est_typ_b);

            if firmware.is_empty() {
                let end = ext
                    .firmware_version
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(ext.firmware_version.len());
                firmware = String::from_utf8_lossy(&ext.firmware_version[..end]).into_owned();
            }
        }
    }

    let warning_flags = generate_warning_flags(pre_eol, life_a, life_b);

    let health = MmcHealth {
        pre_eol_info: pre_eol,
        life_time_est_a_percent: life_a,
        life_time_est_b_percent: life_b,
        warning_flags,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mmc_ioc_cmd_struct_size() {
        assert_eq!(std::mem::size_of::<MmcIocCmd>(), 72);
        // `data_ptr` must sit at offset 64 for 8-byte alignment (kernel
        // comment: the struct must be the same size on 32/64-bit).
        assert_eq!(
            std::mem::offset_of!(MmcIocCmd, data_ptr),
            64,
            "MmcIocCmd.data_ptr must be 8-byte aligned at offset 64"
        );
    }

    #[test]
    fn test_parse_ext_csd() {
        let mut buf = [0u8; 512];
        buf[192] = 8;
        buf[212..216].copy_from_slice(&122_142_720u32.to_le_bytes());
        let fw: &[u8] = b"0x01\0\0\0\0";
        buf[254..262].copy_from_slice(fw);
        buf[267] = 0x02;
        buf[268] = 0x05;
        buf[269] = 0x06;

        let ext = parse_ext_csd(&buf);
        assert_eq!(ext.rev, 8);
        assert_eq!(ext.sec_count, 122_142_720);
        assert_eq!(ext.firmware_version, [b'0', b'x', b'0', b'1', 0, 0, 0, 0]);
        assert_eq!(ext.pre_eol_info, 0x02);
        assert_eq!(ext.life_time_est_typ_a, 0x05);
        assert_eq!(ext.life_time_est_typ_b, 0x06);
    }

    #[test]
    fn test_read_ext_csd_raw_nonexistent_device() {
        let res = read_ext_csd_raw("/dev/nonexistent_mmc_device_xyz");
        assert!(matches!(res, Err(MmcError::Io(_))));
    }
}
