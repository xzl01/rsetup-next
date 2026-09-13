use super::{NvmeError, parse_smart_log};
use crate::model::{NvmeDevice, NvmeSmartLog};
use std::fs;
use std::path::{Path, PathBuf};

#[repr(C)]
struct NvmePassthruCmd {
    opcode: u8,
    flags: u8,
    rsvd1: u16,
    nsid: u32,
    cdw2: u32,
    cdw3: u32,
    metadata: u64,
    addr: u64,
    metadata_len: u32,
    data_len: u32,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
    timeout_ms: u32,
    result: u32,
}

// _IOWR('N', 0x41, struct nvme_passthru_cmd)
// 'N' = 0x4e, 0x41, size = 72 (0x48), dir = _IOC_READ|_IOC_WRITE = 3
// 3 << 30 | 72 << 16 | 0x4e << 8 | 0x41 = 0xc0484e41
const NVME_IOCTL_ADMIN_CMD: libc::c_ulong = 0xc0484e41;
const NVME_ADMIN_OPCODE_GET_LOG_PAGE: u8 = 0x02;
const NVME_LOG_LID_SMART: u32 = 0x02;

/// Check result of NVMe admin command.
/// ret == 0: success
/// ret > 0: NVMe completion status (command error)
/// ret < 0: errno from ioctl failure
pub(crate) fn check_admin_result(ret: i32, errno: i32) -> Result<(), NvmeError> {
    match ret {
        0 => Ok(()),
        n if n > 0 => Err(NvmeError::CommandStatus(n)),
        _ => Err(NvmeError::IoCode(errno)),
    }
}

/// Read a trimmed string from a file if it exists.
fn read_trimmed_attr(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Calculate total capacity in bytes from namespaces under a controller directory.
fn read_namespaces_total_bytes(ctrl_dir: &Path) -> u64 {
    let mut total_bytes = 0u64;
    if let Ok(entries) = fs::read_dir(ctrl_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            // Match namespace directories like nvme0n1
            if name.contains('n') && entry.path().is_dir() {
                let size_file = entry.path().join("size");
                if let Ok(size_str) = fs::read_to_string(&size_file) {
                    if let Ok(blocks) = size_str.trim().parse::<u64>() {
                        // Standard block size for sysfs block layer size attribute is 512 bytes
                        total_bytes = total_bytes.saturating_add(blocks.saturating_mul(512));
                    }
                }
            }
        }
    }
    total_bytes
}

/// Read sysfs controller info and construct an NvmeDevice.
pub fn read_controller_sysfs(sysfs_root: &Path, ctrl_name: &str) -> Result<NvmeDevice, NvmeError> {
    if sysfs_root == Path::new("/") {
        read_controller_sysfs_with(sysfs_root, ctrl_name, &read_smart_log_raw)
    } else {
        read_controller_sysfs_with(sysfs_root, ctrl_name, &|_| {
            Err(NvmeError::NotSupported(
                "fixture requires an injected reader".into(),
            ))
        })
    }
}

pub(crate) fn read_controller_sysfs_with(
    root: &Path,
    name: &str,
    reader: &dyn Fn(&str) -> Result<[u8; 512], NvmeError>,
) -> Result<NvmeDevice, NvmeError> {
    let ctrl_dir = if root == Path::new("/") {
        PathBuf::from(format!("/sys/class/nvme/{}", name))
    } else {
        root.join(format!("sys/class/nvme/{}", name))
    };

    if !ctrl_dir.exists() {
        return Err(NvmeError::Io(format!(
            "Controller directory does not exist: {}",
            ctrl_dir.display()
        )));
    }

    let model = read_trimmed_attr(&ctrl_dir.join("model")).unwrap_or_default();
    let serial = read_trimmed_attr(&ctrl_dir.join("serial")).unwrap_or_default();
    let firmware = read_trimmed_attr(&ctrl_dir.join("firmware_rev")).unwrap_or_default();
    let total_bytes = read_namespaces_total_bytes(&ctrl_dir);

    let dev_path = format!("/dev/{}", name);
    let smart = reader(&dev_path)
        .and_then(|buf| parse_smart_log(&buf))
        .unwrap_or_default();

    Ok(NvmeDevice {
        name: name.to_string(),
        path: dev_path,
        model,
        serial,
        firmware,
        total_bytes,
        smart,
    })
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

/// Read 512-byte SMART/Health log buffer from an NVMe device node via direct Admin passthru ioctl.
pub fn read_smart_log_raw(dev_path: &str) -> Result<[u8; 512], NvmeError> {
    use std::ffi::CString;

    let c_path = CString::new(dev_path)
        .map_err(|e| NvmeError::Io(format!("Invalid device path {}: {}", dev_path, e)))?;

    // Open read-only with O_CLOEXEC, managed by RAII
    let fd = SafeFd::open_read_only(&c_path)
        .map_err(|errno| NvmeError::IoCode(errno))?;

    let mut buf = [0u8; 512];
    let num_dwords = (512 / 4) - 1; // 0-based number of Dwords: 127
    let cdw10 = NVME_LOG_LID_SMART | ((num_dwords as u32) << 16);

    let mut cmd = NvmePassthruCmd {
        opcode: NVME_ADMIN_OPCODE_GET_LOG_PAGE,
        flags: 0,
        rsvd1: 0,
        nsid: 0xFFFFFFFF, // Controller-level SMART log
        cdw2: 0,
        cdw3: 0,
        metadata: 0,
        addr: buf.as_mut_ptr() as u64,
        metadata_len: 0,
        data_len: 512,
        cdw10,
        cdw11: 0,
        cdw12: 0,
        cdw13: 0,
        cdw14: 0,
        cdw15: 0,
        timeout_ms: 0,
        result: 0,
    };

    let ret = unsafe { libc::ioctl(fd.as_raw_fd(), NVME_IOCTL_ADMIN_CMD, &mut cmd) };
    let captured_errno = if ret < 0 {
        unsafe { *libc::__errno_location() }
    } else {
        0
    };

    check_admin_result(ret, captured_errno)?;

    Ok(buf)
}

/// Read and parse SMART log from device path.
pub fn read_smart_log(dev_path: &str) -> Result<NvmeSmartLog, NvmeError> {
    let buf = read_smart_log_raw(dev_path)?;
    parse_smart_log(&buf)
}
