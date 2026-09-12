# Task 2.2 Review Package
Base: 9237ef6885bf24b575e8374cc5ea3b1eac670812
Head: aa5fa98da8ed8d91c231a7f1327900334e703533

## Git Log
aa5fa98 feat(core): add MMC_IOC_CMD ioctl fallback for EXT_CSD health data

## Git Diff --stat
 crates/rsetup-core/src/mmc.rs     |  60 +++++++++-
 crates/rsetup-core/src/mmc/sys.rs | 232 +++++++++++++++++++++++++++++++++++---
 2 files changed, 274 insertions(+), 18 deletions(-)

## Git Diff
diff --git a/crates/rsetup-core/src/mmc.rs b/crates/rsetup-core/src/mmc.rs
index 44e505b..f105e43 100644
--- a/crates/rsetup-core/src/mmc.rs
+++ b/crates/rsetup-core/src/mmc.rs
@@ -106,37 +106,45 @@ impl MmcManager {
 
 fn parse_hex_or_dec_u8(s: &str) -> Option<u8> {
     let s = s.trim();
     if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
         u8::from_str_radix(hex_str, 16).ok()
     } else {
         s.parse::<u8>().ok()
     }
 }
 
-fn map_life_time_val(val: u8) -> Option<u8> {
-    match val {
+/// Map a raw EXT_CSD / sysfs life-time estimate byte to a percentage.
+///
+/// Per the eMMC specification: `0x00` is "Not defined" and `0x01..=0x0A`
+/// represent 10%..100% wear. `0x0B` means the estimated lifetime has been
+/// exceeded (reported as 101 to distinguish it from an exact 100%); all
+/// other values are reserved/undefined and yield `None`.
+pub fn map_life_time_byte_to_percent(byte_val: u8) -> Option<u8> {
+    match byte_val {
         0x00 => None,
-        0x01..=0x0A => Some(val * 10),
+        0x01..=0x0A => Some(byte_val * 10),
         0x0B => Some(101),
         _ => None,
     }
 }
 
 pub fn parse_life_time_str(s: &str) -> (Option<u8>, Option<u8>) {
     let tokens: Vec<&str> = s.split_whitespace().collect();
     if tokens.len() != 2 {
         return (None, None);
     }
 
-    let val_a = parse_hex_or_dec_u8(tokens[0]).and_then(map_life_time_val);
-    let val_b = parse_hex_or_dec_u8(tokens[1]).and_then(map_life_time_val);
+    let val_a = parse_hex_or_dec_u8(tokens[0])
+        .and_then(map_life_time_byte_to_percent);
+    let val_b = parse_hex_or_dec_u8(tokens[1])
+        .and_then(map_life_time_byte_to_percent);
 
     (val_a, val_b)
 }
 
 pub fn parse_pre_eol_info_str(s: &str) -> u8 {
     let val = parse_hex_or_dec_u8(s);
     match val {
         Some(1) => 1,
         Some(2) => 2,
         Some(3) => 3,
@@ -381,20 +389,62 @@ mod tests {
         assert_eq!(d1.card_type, "SD");
         assert_eq!(d1.model, "SC64G");
         assert_eq!(d1.manufacturer, "SanDisk (0x000045)");
         assert_eq!(d1.serial, "0x87654321");
         assert_eq!(d1.block_path, "/dev/mmcblk1");
         assert_eq!(d1.total_bytes, 124735488 * 512);
 
         let _ = std::fs::remove_dir_all(&root);
     }
 
+    #[test]
+    fn test_map_life_time_byte_to_percent() {
+        assert_eq!(map_life_time_byte_to_percent(0x00), None);
+        assert_eq!(map_life_time_byte_to_percent(0x01), Some(10));
+        assert_eq!(map_life_time_byte_to_percent(0x05), Some(50));
+        assert_eq!(map_life_time_byte_to_percent(0x0A), Some(100));
+        assert_eq!(map_life_time_byte_to_percent(0x0B), Some(101));
+        assert_eq!(map_life_time_byte_to_percent(0x0C), None);
+    }
+
+    #[test]
+    fn test_sysfs_fallback_to_ioctl_when_attributes_missing() {
+        let root = std::env::temp_dir().join(format!("rsetup-mmc-fallback-{}", uuid::Uuid::new_v4()));
+        let dev_dir = root.join("sys/bus/mmc/devices/mmc0:0001");
+        std::fs::create_dir_all(&dev_dir).expect("create dev_dir");
+        std::fs::write(dev_dir.join("type"), "MMC\n").unwrap();
+        std::fs::write(dev_dir.join("name"), "FALLBACK\n").unwrap();
+        std::fs::write(dev_dir.join("manfid"), "0x000015\n").unwrap();
+        std::fs::write(dev_dir.join("serial"), "0xDEADBEEF\n").unwrap();
+        // Deliberately NO life_time / pre_eol_info / fwrev / prv / hwrev files:
+        // this forces the ioctl fallback path in read_device_sysfs.
+        let blk_dir = dev_dir.join("block/mmcblk0");
+        std::fs::create_dir_all(&blk_dir).expect("create blk_dir");
+        let class_blk0 = root.join("sys/class/block/mmcblk0");
+        std::fs::create_dir_all(&class_blk0).expect("create class_blk0");
+        std::fs::write(class_blk0.join("size"), "122142720\n").unwrap();
+
+        // On the test machine there is no /dev/mmcblk0, so the ioctl fallback
+        // fails; the device must still be built normally without panicking:
+        // health stays 0/None, firmware stays empty, block detection is intact.
+        let dev = sys::read_device_sysfs(&root, "mmc0:0001").expect("read_device_sysfs");
+        assert_eq!(dev.block_path, "/dev/mmcblk0");
+        assert_eq!(dev.total_bytes, 122_142_720u64 * 512);
+        assert_eq!(dev.health.pre_eol_info, 0);
+        assert_eq!(dev.health.life_time_est_a_percent, None);
+        assert_eq!(dev.health.life_time_est_b_percent, None);
+        assert!(dev.health.warning_flags.is_empty());
+        assert_eq!(dev.firmware, "");
+
+        let _ = std::fs::remove_dir_all(&root);
+    }
+
     #[test]
     fn test_mmc_primary_block_device_selection() {
         let root = std::env::temp_dir().join(format!("rsetup-mmc-blk-filter-{}", uuid::Uuid::new_v4()));
         let dev_dir = root.join("sys/bus/mmc/devices/mmc0:0001");
         std::fs::create_dir_all(&dev_dir).expect("create dev_dir");
         std::fs::write(dev_dir.join("type"), "MMC\n").unwrap();
         std::fs::write(dev_dir.join("name"), "TEST_MMC\n").unwrap();
 
         let block_dir = dev_dir.join("block");
         // Create boot0 as the ONLY file first to see if it mistakenly matches
diff --git a/crates/rsetup-core/src/mmc/sys.rs b/crates/rsetup-core/src/mmc/sys.rs
index 2a1d7e1..ecc00d0 100644
--- a/crates/rsetup-core/src/mmc/sys.rs
+++ b/crates/rsetup-core/src/mmc/sys.rs
@@ -1,15 +1,155 @@
-use super::{parse_life_time_str, parse_pre_eol_info_str, format_manufacturer, generate_warning_flags, MmcError};
+use super::{
+    format_manufacturer, generate_warning_flags, map_life_time_byte_to_percent,
+    parse_life_time_str, parse_pre_eol_info_str, MmcError,
+};
 use crate::model::{MmcDevice, MmcHealth};
 use std::fs;
 use std::path::{Path, PathBuf};
 
+/// Mirror of the kernel's `struct mmc_ioc_cmd`
+/// (`include/uapi/linux/mmc/ioctl.h`), field-for-field. The kernel requires
+/// this struct to be exactly 72 bytes with `data_ptr` 8-byte aligned, the
+/// same on 32- and 64-bit, which `#[repr(C)]` with the trailing `u64` gives
+/// us here.
+#[repr(C)]
+struct MmcIocCmd {
+    write_flag: libc::c_int,
+    is_acmd: libc::c_int,
+    opcode: u32,
+    arg: u32,
+    response: [u32; 4],
+    flags: u32,
+    blksz: u32,
+    blocks: u32,
+    postsleep_min_us: u32,
+    postsleep_max_us: u32,
+    data_timeout_ns: u32,
+    cmd_timeout_ms: u32,
+    _pad: u32,
+    data_ptr: u64,
+}
+
+/// `MMC_BLOCK_MAJOR` from `include/uapi/linux/major.h`.
+const MMC_BLOCK_MAJOR: u8 = 179;
+
+// _IOWR(MMC_BLOCK_MAJOR, 0, struct mmc_ioc_cmd)
+// dir = _IOC_READ | _IOC_WRITE = 3, size = 72 (0x48), type = 179 (0xB3), nr = 0
+// 3 << 30 | 72 << 16 | 0xB3 << 8 | 0x00 = 0xc048b300
+const MMC_IOC_CMD: libc::c_ulong =
+    3u64 << 30 | 72u64 << 16 | (MMC_BLOCK_MAJOR as u64) << 8;
+
+/// CMD6 — SEND EXT_CSD (include/linux/mmc/core.h).
+const MMC_OPCODE_SEND_EXT_CSD: u32 = 8;
+
+/// R1 response type: (1<<0) | (1<<2) | (1<<4).
+const MMC_RSP_R1: u32 = 0x15;
+
+/// Command carries an associated data transfer (include/linux/mmc/core.h).
+const MMC_CMD_ADTC: u32 = 1 << 5;
+
+// EXT_CSD byte offsets (include/linux/mmc/mmc.h)
+const EXT_CSD_REV: usize = 192;
+const EXT_CSD_SEC_CNT: usize = 212;
+const EXT_CSD_FIRMWARE_VERSION: usize = 254;
+const EXT_CSD_PRE_EOL_INFO: usize = 267;
+const EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_A: usize = 268;
+const EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_B: usize = 269;
+
+/// Parsed subset of the 512-byte EXT_CSD register (CMD6).
+#[derive(Debug, Clone, PartialEq, Eq)]
+pub struct MmcExtCsd {
+    pub rev: u8,
+    /// Capacity in 512-byte sectors (EXT_CSD[212..216], little-endian).
+    pub sec_count: u32,
+    /// ASCII firmware revision, NUL-padded (EXT_CSD[254..262]).
+    pub firmware_version: [u8; 8],
+    pub pre_eol_info: u8,
+    /// Raw byte value: 0x00 undefined .. 0x0B exceeded.
+    pub life_time_est_typ_a: u8,
+    pub life_time_est_typ_b: u8,
+}
+
+/// Parse a 512-byte EXT_CSD buffer. Pure safe code — no device access.
+pub fn parse_ext_csd(buf: &[u8; 512]) -> MmcExtCsd {
+    let mut firmware_version = [0u8; 8];
+    firmware_version.copy_from_slice(&buf[EXT_CSD_FIRMWARE_VERSION..EXT_CSD_FIRMWARE_VERSION + 8]);
+
+    MmcExtCsd {
+        rev: buf[EXT_CSD_REV],
+        sec_count: u32::from_le_bytes([
+            buf[EXT_CSD_SEC_CNT],
+            buf[EXT_CSD_SEC_CNT + 1],
+            buf[EXT_CSD_SEC_CNT + 2],
+            buf[EXT_CSD_SEC_CNT + 3],
+        ]),
+        firmware_version,
+        pre_eol_info: buf[EXT_CSD_PRE_EOL_INFO],
+        life_time_est_typ_a: buf[EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_A],
+        life_time_est_typ_b: buf[EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_B],
+    }
+}
+
+/// Read the full 512-byte EXT_CSD register from an MMC block device node
+/// (e.g. `/dev/mmcblk0`) via the direct `MMC_IOC_CMD` ioctl. Read-only;
+/// same style as `nvme/sys.rs::read_smart_log_raw`.
+pub fn read_ext_csd_raw(dev_path: &str) -> Result<[u8; 512], MmcError> {
+    use std::ffi::CString;
+
+    let c_path = CString::new(dev_path)
+        .map_err(|e| MmcError::Io(format!("Invalid device path {}: {}", dev_path, e)))?;
+
+    // Open read-only
+    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
+    if fd < 0 {
+        let err = std::io::Error::last_os_error();
+        return Err(MmcError::Io(format!("Failed to open {}: {}", dev_path, err)));
+    }
+
+    let mut buf = [0u8; 512];
+
+    let mut cmd = MmcIocCmd {
+        write_flag: 0, // read
+        is_acmd: 0,
+        opcode: MMC_OPCODE_SEND_EXT_CSD,
+        arg: 0, // EXT_CSD read starts at offset 0
+        response: [0; 4],
+        flags: MMC_RSP_R1 | MMC_CMD_ADTC,
+        blksz: 512,
+        blocks: 1,
+        postsleep_min_us: 0,
+        postsleep_max_us: 0,
+        data_timeout_ns: 0,
+        cmd_timeout_ms: 0,
+        _pad: 0,
+        data_ptr: buf.as_mut_ptr() as u64,
+    };
+
+    let ret = unsafe { libc::ioctl(fd, MMC_IOC_CMD, &mut cmd) };
+    let ioctl_err = if ret < 0 {
+        Some(std::io::Error::last_os_error())
+    } else {
+        None
+    };
+
+    unsafe { libc::close(fd) };
+
+    if let Some(err) = ioctl_err {
+        return Err(MmcError::Io(format!(
+            "MMC_IOC_CMD failed on {}: {}",
+            dev_path, err
+        )));
+    }
+
+    Ok(buf)
+}
+
 /// Read a trimmed string from a file if it exists.
 pub fn read_trimmed_attr(path: &Path) -> Option<String> {
     fs::read_to_string(path).ok().map(|s| s.trim().to_string())
 }
 
 /// Read block device size in bytes from sysfs.
 pub fn read_block_device_size(sysfs_root: &Path, block_name: &str) -> u64 {
     let size_file = if sysfs_root == Path::new("/") {
         PathBuf::from(format!("/sys/class/block/{}/size", block_name))
     } else {
@@ -52,39 +192,30 @@ pub fn read_device_sysfs(sysfs_root: &Path, dev_name: &str) -> Result<MmcDevice,
     let card_type = read_trimmed_attr(&dev_dir.join("type")).unwrap_or_default();
     if card_type == "SDIO" {
         return Err(MmcError::NotSupported("SDIO device ignored".to_string()));
     }
 
     let model = read_trimmed_attr(&dev_dir.join("name")).unwrap_or_default();
     let manfid_raw = read_trimmed_attr(&dev_dir.join("manfid")).unwrap_or_default();
     let manufacturer = format_manufacturer(&manfid_raw);
     let serial = read_trimmed_attr(&dev_dir.join("serial")).unwrap_or_default();
 
-    let firmware = read_trimmed_attr(&dev_dir.join("fwrev"))
+    let mut firmware = read_trimmed_attr(&dev_dir.join("fwrev"))
         .or_else(|| read_trimmed_attr(&dev_dir.join("prv")))
         .or_else(|| read_trimmed_attr(&dev_dir.join("hwrev")))
         .unwrap_or_default();
 
     let life_time_str = read_trimmed_attr(&dev_dir.join("life_time")).unwrap_or_default();
-    let (life_a, life_b) = parse_life_time_str(&life_time_str);
+    let (mut life_a, mut life_b) = parse_life_time_str(&life_time_str);
 
     let pre_eol_str = read_trimmed_attr(&dev_dir.join("pre_eol_info")).unwrap_or_default();
-    let pre_eol = parse_pre_eol_info_str(&pre_eol_str);
-
-    let warning_flags = generate_warning_flags(pre_eol, life_a, life_b);
-
-    let health = MmcHealth {
-        pre_eol_info: pre_eol,
-        life_time_est_a_percent: life_a,
-        life_time_est_b_percent: life_b,
-        warning_flags,
-    };
+    let mut pre_eol = parse_pre_eol_info_str(&pre_eol_str);
 
     // Determine block device
     // Check dev_dir/block or entries matching block/mmcblk*
     let mut block_name: Option<String> = None;
 
     let block_dir = dev_dir.join("block");
     if block_dir.exists() {
         if let Ok(entries) = fs::read_dir(&block_dir) {
             for entry in entries.flatten() {
                 let name = entry.file_name().to_string_lossy().to_string();
@@ -109,22 +240,97 @@ pub fn read_device_sysfs(sysfs_root: &Path, dev_name: &str) -> Result<MmcDevice,
         }
     }
 
     let (block_path, total_bytes) = if let Some(blk) = block_name {
         let size = read_block_device_size(sysfs_root, &blk);
         (format!("/dev/{}", blk), size)
     } else {
         (String::new(), 0)
     };
 
+    // Fallback: when sysfs exposes no life time / pre-EOL info at all, try
+    // reading EXT_CSD directly from the block device via MMC_IOC_CMD. On
+    // failure, silently keep the sysfs results (unwrap_or_default semantics)
+    // — no error logging, device probing is unaffected.
+    if life_a.is_none() && life_b.is_none() && pre_eol == 0 && !block_path.is_empty() {
+        if let Ok(buf) = read_ext_csd_raw(&block_path) {
+            let ext = parse_ext_csd(&buf);
+            pre_eol = ext.pre_eol_info;
+            life_a = map_life_time_byte_to_percent(ext.life_time_est_typ_a);
+            life_b = map_life_time_byte_to_percent(ext.life_time_est_typ_b);
+
+            if firmware.is_empty() {
+                let end = ext
+                    .firmware_version
+                    .iter()
+                    .position(|&b| b == 0)
+                    .unwrap_or(ext.firmware_version.len());
+                firmware = String::from_utf8_lossy(&ext.firmware_version[..end])
+                    .into_owned();
+            }
+        }
+    }
+
+    let warning_flags = generate_warning_flags(pre_eol, life_a, life_b);
+
+    let health = MmcHealth {
+        pre_eol_info: pre_eol,
+        life_time_est_a_percent: life_a,
+        life_time_est_b_percent: life_b,
+        warning_flags,
+    };
+
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
+
+#[cfg(test)]
+mod tests {
+    use super::*;
+
+    #[test]
+    fn test_mmc_ioc_cmd_struct_size() {
+        assert_eq!(std::mem::size_of::<MmcIocCmd>(), 72);
+        // `data_ptr` must sit at offset 64 for 8-byte alignment (kernel
+        // comment: the struct must be the same size on 32/64-bit).
+        assert_eq!(
+            std::mem::offset_of!(MmcIocCmd, data_ptr),
+            64,
+            "MmcIocCmd.data_ptr must be 8-byte aligned at offset 64"
+        );
+    }
+
+    #[test]
+    fn test_parse_ext_csd() {
+        let mut buf = [0u8; 512];
+        buf[192] = 8;
+        buf[212..216].copy_from_slice(&122_142_720u32.to_le_bytes());
+        let fw: &[u8] = b"0x01\0\0\0\0";
+        buf[254..262].copy_from_slice(fw);
+        buf[267] = 0x02;
+        buf[268] = 0x05;
+        buf[269] = 0x06;
+
+        let ext = parse_ext_csd(&buf);
+        assert_eq!(ext.rev, 8);
+        assert_eq!(ext.sec_count, 122_142_720);
+        assert_eq!(ext.firmware_version, [b'0', b'x', b'0', b'1', 0, 0, 0, 0]);
+        assert_eq!(ext.pre_eol_info, 0x02);
+        assert_eq!(ext.life_time_est_typ_a, 0x05);
+        assert_eq!(ext.life_time_est_typ_b, 0x06);
+    }
+
+    #[test]
+    fn test_read_ext_csd_raw_nonexistent_device() {
+        let res = read_ext_csd_raw("/dev/nonexistent_mmc_device_xyz");
+        assert!(matches!(res, Err(MmcError::Io(_))));
+    }
+}
