use super::{
    MmcError, format_manufacturer, generate_warning_flags, map_life_time_byte_to_percent,
    parse_life_time_str, parse_pre_eol_info_str,
};
use crate::model::{
    MmcDevice, MmcHealth, TelemetryError, TelemetryErrorKind,
    TelemetryReadState, TelemetryStatus,
};
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

/// Helper struct for RAII file descriptor management.
struct SafeFd(libc::c_int);

impl SafeFd {
    fn open_read_only(path: &std::ffi::CStr) -> Result<Self, i32> {
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            let errno = unsafe { *libc::__errno_location() };
            Err(errno)
        } else {
            Ok(Self(fd))
        }
    }

    fn as_raw_fd(&self) -> libc::c_int {
        self.0
    }
}

impl Drop for SafeFd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe { libc::close(self.0) };
        }
    }
}

/// Read the full 512-byte EXT_CSD register from an MMC block device node
/// (e.g. `/dev/mmcblk0`) via the direct `MMC_IOC_CMD` ioctl. Read-only;
/// same style as `nvme/sys.rs::read_smart_log_raw`.
pub fn read_ext_csd_raw(dev_path: &str) -> Result<[u8; 512], MmcError> {
    use std::ffi::CString;

    let c_path = CString::new(dev_path)
        .map_err(|e| MmcError::Io(format!("Invalid device path {}: {}", dev_path, e)))?;

    // Open read-only with O_CLOEXEC, managed by RAII
    let fd = SafeFd::open_read_only(&c_path).map_err(MmcError::IoCode)?;

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

    let ret = unsafe { libc::ioctl(fd.as_raw_fd(), MMC_IOC_CMD, &mut cmd) };
    if ret < 0 {
        let errno = unsafe { *libc::__errno_location() };
        return Err(MmcError::IoCode(errno));
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
    if sysfs_root == Path::new("/") {
        read_device_sysfs_with(sysfs_root, dev_name, &read_ext_csd_raw)
    } else {
        read_device_sysfs_with(sysfs_root, dev_name, &|_| {
            Err(MmcError::NotSupported(
                "fixture requires an injected reader".into(),
            ))
        })
    }
}

pub(crate) fn telemetry_from_mmc_error(err: &MmcError) -> TelemetryStatus {
    let error = match err {
        MmcError::IoCode(code) if *code == libc::EACCES || *code == libc::EPERM => {
            Some(TelemetryError {
                kind: TelemetryErrorKind::PermissionDenied,
                code: Some(*code),
            })
        }
        MmcError::IoCode(code) => Some(TelemetryError {
            kind: TelemetryErrorKind::Io,
            code: Some(*code),
        }),
        MmcError::Io(_) | MmcError::NotSupported(_) | MmcError::InvalidBufferLength { .. } => {
            Some(TelemetryError {
                kind: TelemetryErrorKind::Io,
                code: None,
            })
        }
    };
    TelemetryStatus {
        state: TelemetryReadState::Unavailable,
        error,
    }
}

pub(crate) fn read_device_sysfs_with(
    sysfs_root: &Path,
    dev_name: &str,
    reader: &dyn Fn(&str) -> Result<[u8; 512], MmcError>,
) -> Result<MmcDevice, MmcError> {
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

    let telemetry;

    if card_type == "SD" {
        telemetry = TelemetryStatus {
            state: TelemetryReadState::Unsupported,
            error: None,
        };
    } else if pre_eol != 0 || life_a.is_some() || life_b.is_some() {
        telemetry = TelemetryStatus {
            state: TelemetryReadState::Available,
            error: None,
        };
    } else if block_path.is_empty() {
        telemetry = TelemetryStatus {
            state: TelemetryReadState::Unavailable,
            error: Some(TelemetryError {
                kind: TelemetryErrorKind::Io,
                code: None,
            }),
        };
    } else {
        // Fallback: only when card_type == "MMC", all health attributes in sysfs are unknown/missing,
        // and primary block device exists. SD cards must NEVER issue EXT_CSD ioctl.
        match reader(&block_path) {
            Ok(buf) => {
                let ext = parse_ext_csd(&buf);
                if ext.rev < 7 {
                    telemetry = TelemetryStatus {
                        state: TelemetryReadState::Unsupported,
                        error: None,
                    };
                } else {
                    // pre-EOL non-1/2/3 normalized to 0
                    pre_eol = match ext.pre_eol_info {
                        1 => 1,
                        2 => 2,
                        3 => 3,
                        _ => 0,
                    };
                    life_a = map_life_time_byte_to_percent(ext.life_time_est_typ_a);
                    life_b = map_life_time_byte_to_percent(ext.life_time_est_typ_b);
                    telemetry = TelemetryStatus {
                        state: TelemetryReadState::Available,
                        error: None,
                    };
                }

                if firmware.is_empty() {
                    let end = ext
                        .firmware_version
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(ext.firmware_version.len());
                    firmware = String::from_utf8_lossy(&ext.firmware_version[..end]).into_owned();
                }
            }
            Err(err) => {
                telemetry = telemetry_from_mmc_error(&err);
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
    let health_state = crate::mmc_health_state(&telemetry, &health);

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
        telemetry,
        health_state,
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
        assert!(matches!(res, Err(MmcError::Io(_)) | Err(MmcError::IoCode(_))));
    }

    fn card_fixture(card_type: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("rsetup-storage-{}", uuid::Uuid::new_v4()));
        let card = root.join("sys/bus/mmc/devices/mmc0:0001");
        std::fs::create_dir_all(card.join("block/mmcblk0")).unwrap();
        std::fs::write(card.join("type"), card_type).unwrap();
        std::fs::write(card.join("name"), "FIXTURE").unwrap();
        let block = root.join("sys/class/block/mmcblk0");
        std::fs::create_dir_all(&block).unwrap();
        std::fs::write(block.join("size"), "4096").unwrap();
        root
    }

    #[test]
    fn sd_never_calls_ext_csd_reader() {
        let root = card_fixture("SD");
        let forbidden = |_: &str| -> Result<[u8; 512], MmcError> {
            panic!("SD must not issue eMMC CMD8");
        };
        let device = read_device_sysfs_with(&root, "mmc0:0001", &forbidden).unwrap();
        assert_eq!(device.card_type, "SD");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ext_csd_invocation_counts_and_parser_matrix() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // 1. MMC missing health attributes -> calls reader exactly 1 time, parses successfully
        {
            let root = card_fixture("MMC");
            let count = AtomicUsize::new(0);
            let fake_reader = |path: &str| -> Result<[u8; 512], MmcError> {
                assert_eq!(path, "/dev/mmcblk0");
                count.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 512];
                buf[192] = 7;    // rev = 7
                buf[267] = 2;    // pre_eol_info = 2
                buf[268] = 0x0A; // life_a = 0x0A -> 100%
                buf[269] = 0x0B; // life_b = 0x0B -> 101%
                Ok(buf)
            };
            let dev = read_device_sysfs_with(&root, "mmc0:0001", &fake_reader).unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 1);
            assert_eq!(dev.health.pre_eol_info, 2);
            assert_eq!(dev.health.life_time_est_a_percent, Some(100));
            assert_eq!(dev.health.life_time_est_b_percent, Some(101));
            assert_eq!(
                dev.health.warning_flags,
                vec!["pre_eol_warning".to_string(), "life_time_typ_b_exceeded".to_string()]
            );
            std::fs::remove_dir_all(root).unwrap();
        }

        // 2. MMC with valid life_time in sysfs -> 0 calls to reader
        {
            let root = card_fixture("MMC");
            let card = root.join("sys/bus/mmc/devices/mmc0:0001");
            std::fs::write(card.join("life_time"), "0x01 0x01\n").unwrap();
            let count = AtomicUsize::new(0);
            let reader = |_: &str| -> Result<[u8; 512], MmcError> {
                count.fetch_add(1, Ordering::SeqCst);
                Ok([0u8; 512])
            };
            let dev = read_device_sysfs_with(&root, "mmc0:0001", &reader).unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 0);
            assert_eq!(dev.health.life_time_est_a_percent, Some(10));
            assert_eq!(dev.health.life_time_est_b_percent, Some(10));
            std::fs::remove_dir_all(root).unwrap();
        }

        // 3. MMC with valid pre_eol_info in sysfs -> 0 calls to reader
        {
            let root = card_fixture("MMC");
            let card = root.join("sys/bus/mmc/devices/mmc0:0001");
            std::fs::write(card.join("pre_eol_info"), "1\n").unwrap();
            let count = AtomicUsize::new(0);
            let reader = |_: &str| -> Result<[u8; 512], MmcError> {
                count.fetch_add(1, Ordering::SeqCst);
                Ok([0u8; 512])
            };
            let dev = read_device_sysfs_with(&root, "mmc0:0001", &reader).unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 0);
            assert_eq!(dev.health.pre_eol_info, 1);
            std::fs::remove_dir_all(root).unwrap();
        }

        // 4. SD -> 0 calls to reader
        {
            let root = card_fixture("SD");
            let count = AtomicUsize::new(0);
            let reader = |_: &str| -> Result<[u8; 512], MmcError> {
                count.fetch_add(1, Ordering::SeqCst);
                Ok([0u8; 512])
            };
            let dev = read_device_sysfs_with(&root, "mmc0:0001", &reader).unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 0);
            assert_eq!(dev.card_type, "SD");
            std::fs::remove_dir_all(root).unwrap();
        }

        // 5. SDIO -> filtered out before fallback (Err NotSupported), 0 calls
        {
            let root = card_fixture("SDIO");
            let count = AtomicUsize::new(0);
            let reader = |_: &str| -> Result<[u8; 512], MmcError> {
                count.fetch_add(1, Ordering::SeqCst);
                Ok([0u8; 512])
            };
            let res = read_device_sysfs_with(&root, "mmc0:0001", &reader);
            assert_eq!(count.load(Ordering::SeqCst), 0);
            assert!(matches!(res, Err(MmcError::NotSupported(_))));
            std::fs::remove_dir_all(root).unwrap();
        }

        // 6. MMC with no block device -> 0 calls to reader
        {
            let root = std::env::temp_dir().join(format!("rsetup-storage-{}", uuid::Uuid::new_v4()));
            let card = root.join("sys/bus/mmc/devices/mmc0:0001");
            std::fs::create_dir_all(&card).unwrap();
            std::fs::write(card.join("type"), "MMC").unwrap();
            std::fs::write(card.join("name"), "NO_BLOCK").unwrap();
            let count = AtomicUsize::new(0);
            let reader = |_: &str| -> Result<[u8; 512], MmcError> {
                count.fetch_add(1, Ordering::SeqCst);
                Ok([0u8; 512])
            };
            let dev = read_device_sysfs_with(&root, "mmc0:0001", &reader).unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 0);
            assert_eq!(dev.block_path, "");
            std::fs::remove_dir_all(root).unwrap();
        }

        // 7. MMC with only boot / rpmb / partition -> does not select as primary block, 0 calls
        {
            let root = std::env::temp_dir().join(format!("rsetup-storage-{}", uuid::Uuid::new_v4()));
            let card = root.join("sys/bus/mmc/devices/mmc0:0001");
            std::fs::create_dir_all(card.join("block/mmcblk0boot0")).unwrap();
            std::fs::create_dir_all(card.join("block/mmcblk0rpmb")).unwrap();
            std::fs::create_dir_all(card.join("block/mmcblk0p1")).unwrap();
            std::fs::write(card.join("type"), "MMC").unwrap();
            let count = AtomicUsize::new(0);
            let reader = |_: &str| -> Result<[u8; 512], MmcError> {
                count.fetch_add(1, Ordering::SeqCst);
                Ok([0u8; 512])
            };
            let dev = read_device_sysfs_with(&root, "mmc0:0001", &reader).unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 0);
            assert_eq!(dev.block_path, "");
            std::fs::remove_dir_all(root).unwrap();
        }

        // 8. rev < 7 does not interpret health bytes; rev >= 7 interprets; pre-EOL non-1/2/3 normalized to 0; reserved lifetime is None
        {
            let root = card_fixture("MMC");
            let reader_rev6 = |_: &str| -> Result<[u8; 512], MmcError> {
                let mut buf = [0u8; 512];
                buf[192] = 6;    // rev < 7
                buf[267] = 2;
                buf[268] = 0x05;
                buf[269] = 0x06;
                Ok(buf)
            };
            let dev_rev6 = read_device_sysfs_with(&root, "mmc0:0001", &reader_rev6).unwrap();
            assert_eq!(dev_rev6.health.pre_eol_info, 0);
            assert_eq!(dev_rev6.health.life_time_est_a_percent, None);
            assert_eq!(dev_rev6.health.life_time_est_b_percent, None);

            // rev 7 with invalid pre_eol (e.g. 4) and reserved lifetime (0x0C)
            let reader_rev7_reserved = |_: &str| -> Result<[u8; 512], MmcError> {
                let mut buf = [0u8; 512];
                buf[192] = 7;
                buf[267] = 4;    // invalid pre-EOL -> normalized to 0
                buf[268] = 0x0C; // reserved -> None
                buf[269] = 0x00; // not defined -> None
                Ok(buf)
            };
            let dev_rev7 = read_device_sysfs_with(&root, "mmc0:0001", &reader_rev7_reserved).unwrap();
            assert_eq!(dev_rev7.health.pre_eol_info, 0);
            assert_eq!(dev_rev7.health.life_time_est_a_percent, None);
            assert_eq!(dev_rev7.health.life_time_est_b_percent, None);

            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
