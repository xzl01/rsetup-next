//! The live backend: enumerating whole disks and their partitions from
//! `sys/class/block`, reading mount facts from the injected root, writing
//! chunks through an `O_EXCL` handle cached per target, and asking the kernel
//! (via `BLKRRPART`) to re-read a partition table.

use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::model::{BlockDeviceKind, SystemInstallPartition, SystemInstallTarget};

use super::error::SystemInstallError;
use super::mounts::{expand_mounts, parse_mounts, partition_number_of, read_attr};
use super::{BlockDeviceBackend, SECTOR_BYTES, SystemMount};

/// Virtual devices an image must never be written to.
const VIRTUAL_DEVICE_PREFIXES: [&str; 7] = ["loop", "ram", "zram", "sr", "dm-", "md", "fd"];

/// `ioctl` that asks the kernel to re-read a block device's partition table.
/// Linux encodes it as `_IO(0x12, 81)`; `libc` does not export the name.
#[cfg(target_os = "linux")]
const BLKRRPART: libc::c_ulong = 0x1251;

/// Live backend. Everything it touches is resolved under `root`, so tests can
/// drive it with fixtures and can never reach a real device node.
pub struct SystemBlockBackend {
    root: PathBuf,
    handles: Mutex<Option<(String, File)>>,
}

impl SystemBlockBackend {
    pub fn new(root: Option<&Path>) -> Self {
        Self {
            root: root.unwrap_or_else(|| Path::new("/")).to_path_buf(),
            handles: Mutex::new(None),
        }
    }

    fn sys_block(&self) -> PathBuf {
        self.root.join("sys/class/block")
    }

    /// The device node under the injected root. Joining a relative path keeps
    /// an injected root authoritative, exactly like the mmc/nvme sysfs readers.
    fn device_path(&self, name: &str) -> PathBuf {
        self.root.join("dev").join(name)
    }
}

impl BlockDeviceBackend for SystemBlockBackend {
    fn targets(&self) -> Result<Vec<SystemInstallTarget>, SystemInstallError> {
        let class = self.sys_block();
        let entries = fs::read_dir(&class).map_err(|error| {
            SystemInstallError::Unsupported(format!("{} is unavailable: {error}", class.display()))
        })?;
        let mut disks: Vec<RawDisk> = Vec::new();
        let mut partitions: Vec<RawPartition> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let size_sectors = read_attr(&path.join("size"))
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            if let Some(number) =
                read_attr(&path.join("partition")).and_then(|value| value.parse::<u32>().ok())
            {
                partitions.push(RawPartition {
                    name,
                    number,
                    start_bytes: read_attr(&path.join("start"))
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(0)
                        * SECTOR_BYTES,
                    size_bytes: size_sectors * SECTOR_BYTES,
                });
                continue;
            }
            if VIRTUAL_DEVICE_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                continue;
            }
            disks.push(RawDisk {
                kind: classify(&path, &name),
                removable: read_attr(&path.join("removable")).as_deref() == Some("1"),
                model: read_attr(&path.join("device/model"))
                    .or_else(|| read_attr(&path.join("device/name")))
                    .unwrap_or_default(),
                name,
                size_bytes: size_sectors * SECTOR_BYTES,
            });
        }
        let mut targets = Vec::new();
        for disk in disks {
            let mut owned = partitions
                .iter()
                .filter(|partition| partition_number_of(&disk.name, &partition.name).is_some())
                .map(|partition| SystemInstallPartition {
                    name: partition.name.clone(),
                    number: partition.number,
                    start_bytes: partition.start_bytes,
                    size_bytes: partition.size_bytes,
                    mount_point: None,
                })
                .collect::<Vec<_>>();
            owned.sort_by_key(|partition| partition.number);
            targets.push(SystemInstallTarget {
                path: format!("/dev/{}", disk.name),
                id: disk.name,
                model: disk.model,
                size_bytes: disk.size_bytes,
                kind: disk.kind,
                removable: disk.removable,
                mounted: false,
                mount_points: Vec::new(),
                holds_running_system: false,
                partitions: owned,
            });
        }
        Ok(targets)
    }

    fn mounts(&self) -> Result<Vec<SystemMount>, SystemInstallError> {
        let path = self.root.join("proc/mounts");
        let text = fs::read_to_string(&path).map_err(|error| {
            SystemInstallError::Unsupported(format!("{} is unavailable: {error}", path.display()))
        })?;
        let mut mounts = parse_mounts(&text);
        expand_mounts(&self.root, &mut mounts)?;
        Ok(mounts)
    }

    fn lock_root(&self) -> Option<PathBuf> {
        Some(self.root.clone())
    }

    fn write_chunk(
        &self,
        target: &SystemInstallTarget,
        offset: u64,
        chunk: &[u8],
    ) -> Result<(), SystemInstallError> {
        let mut handles = self.handles.lock().expect("device handle lock");
        let needs_open = handles
            .as_ref()
            .map(|(path, _)| path != &target.id)
            .unwrap_or(true);
        if needs_open {
            let path = self.device_path(&target.id);
            // O_EXCL asks the kernel for an exclusive open, so a device another
            // process already holds cannot be written behind its back. On a
            // regular file (a fixture root) the flag has no effect.
            let file = OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_EXCL)
                .open(&path)
                .map_err(|error| match error.raw_os_error() {
                    Some(libc::EBUSY) => SystemInstallError::Conflict(format!(
                        "{} is already open by another process",
                        path.display()
                    )),
                    _ => SystemInstallError::Io(format!(
                        "unable to open {}: {error}",
                        path.display()
                    )),
                })?;
            *handles = Some((target.id.clone(), file));
        }
        let (_, file) = handles.as_mut().expect("device handle opened");
        let path = self.device_path(&target.id);
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.write_all(chunk))
            .map_err(|error| {
                SystemInstallError::Io(format!("unable to write {}: {error}", path.display()))
            })
    }

    fn sync_device(&self, _target: &SystemInstallTarget) -> Result<(), SystemInstallError> {
        let mut handles = self.handles.lock().expect("device handle lock");
        if let Some((_, file)) = handles.as_mut() {
            file.sync_all()
                .map_err(|error| SystemInstallError::Io(format!("unable to flush: {error}")))?;
        }
        *handles = None;
        Ok(())
    }

    fn reread_partitions(
        &self,
        target: &SystemInstallTarget,
    ) -> Result<Vec<SystemInstallPartition>, SystemInstallError> {
        reread_partition_table(&self.device_path(&target.id))?;
        let targets = self.targets()?;
        targets
            .into_iter()
            .find(|candidate| candidate.id == target.id)
            .map(|candidate| candidate.partitions)
            .ok_or_else(|| {
                SystemInstallError::InvalidInput(format!(
                    "{} disappeared while it was being written",
                    target.path
                ))
            })
    }
}

struct RawDisk {
    name: String,
    model: String,
    size_bytes: u64,
    removable: bool,
    kind: BlockDeviceKind,
}

struct RawPartition {
    name: String,
    number: u32,
    start_bytes: u64,
    size_bytes: u64,
}

/// Kernel attributes decide the physical kind. UFS is not named by the kernel,
/// so it is recognised by its host controller path (`ufs`); an SD card reports
/// `SD` as its MMC type. Real-device validation of these heuristics is pending
/// hardware access and is listed as such in the delivery notes.
fn classify(entry: &Path, name: &str) -> BlockDeviceKind {
    if name.starts_with("nvme") {
        return BlockDeviceKind::Nvme;
    }
    if name.starts_with("mmcblk") {
        return match read_attr(&entry.join("device/type")).as_deref() {
            Some("SD") => BlockDeviceKind::Sd,
            Some(_) => BlockDeviceKind::Emmc,
            None => BlockDeviceKind::Other,
        };
    }
    let host_path = fs::read_link(entry)
        .map(|target| target.to_string_lossy().into_owned())
        .unwrap_or_default();
    if host_path.contains("ufs") {
        BlockDeviceKind::Ufs
    } else {
        BlockDeviceKind::Other
    }
}

/// Linux asks the kernel for the new partition table; any other host reports
/// that the check is unavailable instead of pretending it passed.
#[cfg(target_os = "linux")]
fn reread_partition_table(path: &Path) -> Result<(), SystemInstallError> {
    use std::os::unix::io::AsRawFd;

    let file = OpenOptions::new().read(true).open(path).map_err(|error| {
        SystemInstallError::Io(format!("unable to open {}: {error}", path.display()))
    })?;
    // SAFETY: BLKRRPART takes no argument beyond the descriptor, which `file`
    // owns for the duration of the call.
    if unsafe { libc::ioctl(file.as_raw_fd(), BLKRRPART) } != 0 {
        return Err(SystemInstallError::Io(format!(
            "the kernel did not re-read {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn reread_partition_table(path: &Path) -> Result<(), SystemInstallError> {
    Err(SystemInstallError::Unsupported(format!(
        "re-reading the partition table of {} (BLKRRPART) is only implemented on Linux",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SystemInstallRequest;
    use crate::system_install::test_support::*;
    use crate::system_install::{MIB, SystemInstallManager, SystemInstallVerificationState};

    #[test]
    fn enumerates_devices_from_an_injected_root() {
        let root = scratch("enumerate");
        // A UFS disk sits on a UFS host controller, so its class entry is a
        // symlink whose target path names that controller.
        fs::create_dir_all(root.join("sys/class/block")).expect("create class directory");
        let ufs = root.join("devices/platform/soc/11300000.ufs/host0/block/sda");
        write_attr(&ufs.join("size"), "250069680\n");
        write_attr(&ufs.join("removable"), "0\n");
        write_attr(&ufs.join("device/model"), "RADXA UFS 128GB\n");
        write_attr(&ufs.join("sda1/partition"), "1\n");
        write_attr(&ufs.join("sda1/start"), "2048\n");
        write_attr(&ufs.join("sda1/size"), "262144\n");
        std::os::unix::fs::symlink(
            "../../../devices/platform/soc/11300000.ufs/host0/block/sda",
            root.join("sys/class/block/sda"),
        )
        .expect("symlink the ufs host");
        std::os::unix::fs::symlink(
            "../../../devices/platform/soc/11300000.ufs/host0/block/sda/sda1",
            root.join("sys/class/block/sda1"),
        )
        .expect("symlink the ufs partition");
        write_attr(&root.join("sys/class/block/mmcblk0/size"), "122142720\n");
        write_attr(&root.join("sys/class/block/mmcblk0/removable"), "0\n");
        write_attr(
            &root.join("sys/class/block/mmcblk0/device/name"),
            "AJTD4R\n",
        );
        write_attr(&root.join("sys/class/block/mmcblk0/device/type"), "MMC\n");
        write_attr(&root.join("sys/class/block/mmcblk1/size"), "62333952\n");
        write_attr(&root.join("sys/class/block/mmcblk1/removable"), "1\n");
        write_attr(&root.join("sys/class/block/mmcblk1/device/name"), "SD32G\n");
        write_attr(&root.join("sys/class/block/mmcblk1/device/type"), "SD\n");
        write_attr(&root.join("sys/class/block/mmcblk1/dev"), "179:0\n");
        write_attr(&root.join("sys/class/block/mmcblk1p1/dev"), "179:1\n");
        write_attr(&root.join("sys/class/block/mmcblk1p1/partition"), "1\n");
        write_attr(&root.join("sys/class/block/mmcblk1p1/start"), "8192\n");
        write_attr(&root.join("sys/class/block/mmcblk1p1/size"), "524288\n");
        write_attr(&root.join("sys/class/block/mmcblk1p2/dev"), "179:2\n");
        write_attr(&root.join("sys/class/block/mmcblk1p2/partition"), "2\n");
        write_attr(&root.join("sys/class/block/mmcblk1p2/start"), "540672\n");
        write_attr(&root.join("sys/class/block/mmcblk1p2/size"), "819200\n");
        write_attr(&root.join("sys/class/block/mmcblk0/dev"), "179:32\n");
        write_attr(&root.join("sys/class/block/loop0/size"), "1024\n");
        write_file(
            &root.join("proc/mounts"),
            b"/dev/mmcblk1p1 /boot vfat rw 0 0\n/dev/mmcblk1p2 / ext4 rw 0 0\n",
        );
        // The kernel's own table carries the device number for each mount, which
        // is how a `/dev/root` or by-uuid source is attributed to its disk.
        write_file(
            &root.join("proc/self/mountinfo"),
            b"36 35 179:1 / /boot rw,relatime - vfat /dev/mmcblk1p1 rw\n37 35 179:2 / / rw,relatime - ext4 /dev/mmcblk1p2 rw\n",
        );

        let manager = SystemInstallManager::new(Some(&root), false);
        let targets = manager.targets().expect("enumerate fixture devices");
        assert_eq!(
            targets
                .iter()
                .map(|target| target.id.as_str())
                .collect::<Vec<_>>(),
            vec!["mmcblk0", "mmcblk1", "sda"],
            "loop devices are skipped and the order is stable"
        );
        let ufs = &targets[2];
        assert_eq!(ufs.kind, BlockDeviceKind::Ufs);
        assert_eq!(ufs.model, "RADXA UFS 128GB");
        assert_eq!(ufs.size_bytes, 250_069_680 * SECTOR_BYTES);
        assert_eq!(ufs.partitions.len(), 1);
        assert_eq!(ufs.partitions[0].name, "sda1");
        assert_eq!(ufs.partitions[0].start_bytes, 2048 * SECTOR_BYTES);
        let emmc = &targets[0];
        assert_eq!(emmc.kind, BlockDeviceKind::Emmc);
        assert_eq!(emmc.model, "AJTD4R");
        assert!(!emmc.removable);
        let sd = &targets[1];
        assert_eq!(sd.kind, BlockDeviceKind::Sd);
        assert!(sd.removable);
        assert!(
            sd.holds_running_system,
            "the fixture boots from the SD card"
        );
        assert!(sd.mounted);
        assert_eq!(sd.mount_points, vec!["/", "/boot"]);
        assert_eq!(sd.partitions[0].mount_point.as_deref(), Some("/boot"));
        assert!(!ufs.holds_running_system);
    }

    #[test]
    fn the_live_backend_writes_only_under_the_injected_root() {
        let root = scratch("live-root");
        fs::create_dir_all(root.join("sys/class/block")).expect("create class directory");
        let ufs = root.join("devices/platform/soc/ufs/host0/block/sda");
        write_attr(&ufs.join("size"), "131072\n");
        write_attr(&ufs.join("removable"), "0\n");
        write_attr(&ufs.join("device/model"), "FIXTURE UFS\n");
        std::os::unix::fs::symlink(
            "../../../devices/platform/soc/ufs/host0/block/sda",
            root.join("sys/class/block/sda"),
        )
        .expect("symlink ufs host");
        write_file(&root.join("dev/sda"), vec![0u8; 4 * MIB as usize]);
        write_attr(&root.join("sys/class/block/sda/dev"), "8:0\n");
        // The running system is on the SD card, named through a source that is not
        // a kernel device name, so only the mount table attributes it correctly.
        write_attr(&root.join("sys/class/block/mmcblk1/dev"), "179:0\n");
        write_attr(&root.join("sys/class/block/mmcblk1p2/dev"), "179:2\n");
        write_file(&root.join("proc/mounts"), b"/dev/root / ext4 rw 0 0\n");
        write_file(
            &root.join("proc/self/mountinfo"),
            b"37 35 179:2 / / rw,relatime - ext4 /dev/root rw\n",
        );

        let image = root.join("system.img");
        write_file(&image, mbr_image(1, 2 * MIB));
        let manager = SystemInstallManager::new(Some(&root), false);
        let plan = manager
            .plan(&SystemInstallRequest {
                target_id: "sda".into(),
                image_path: Some(image.display().to_string()),
                image_id: None,
            })
            .expect("plan against the fixture root");
        let result = manager
            .apply_live(
                &SystemInstallRequest {
                    target_id: "sda".into(),
                    image_path: Some(image.display().to_string()),
                    image_id: None,
                },
                &plan.plan_token,
                "sda",
            )
            .expect("write into the fixture root");
        assert_eq!(result.bytes_written, plan.image.uncompressed_bytes);
        assert_eq!(
            fs::read(root.join("dev/sda")).expect("read the fixture device"),
            fs::read(&image).expect("read the image"),
            "the fixture device holds the image and no real device was touched"
        );
        assert_ne!(
            result.verification.state,
            SystemInstallVerificationState::Matched,
            "re-reading is unsupported here (macOS) or fails on a plain file (Linux)"
        );
    }
}
