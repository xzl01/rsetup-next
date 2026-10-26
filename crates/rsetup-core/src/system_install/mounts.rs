//! Mount and partition occupancy probes: parsing the mount tables, resolving a
//! mount source to the kernel block device names it really lives on (following
//! the slave chain so LVM and md are attributed to the disk underneath), and
//! attributing the result to install targets for the safety gates.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::model::SystemInstallTarget;

use super::SystemMount;
use super::error::SystemInstallError;

/// Marks a target with the mount facts the gates need.
pub(super) fn annotate_mounts(target: &mut SystemInstallTarget, mounts: &[SystemMount]) {
    let mut mount_points = Vec::new();
    let mut holds_running_system = false;
    for mount in mounts {
        if !resolves_to_target(&mount.device, target) {
            continue;
        }
        if !mount_points.contains(&mount.mount_point) {
            mount_points.push(mount.mount_point.clone());
        }
        if is_running_system_mount(&mount.mount_point) {
            holds_running_system = true;
        }
    }
    mount_points.sort();
    target.mounted = !mount_points.is_empty();
    target.mount_points = mount_points;
    target.holds_running_system = holds_running_system;
    for partition in &mut target.partitions {
        partition.mount_point = mounts
            .iter()
            .find(|mount| mount.device.trim_start_matches("/dev/") == partition.name)
            .map(|mount| mount.mount_point.clone());
    }
}

fn is_running_system_mount(mount_point: &str) -> bool {
    mount_point == "/" || mount_point == "/boot" || mount_point.starts_with("/boot/")
}

/// Rewrites every mount source into the kernel block device names it lives on.
///
/// `/proc/mounts` reports the source the kernel was handed, which is frequently
/// not a kernel device name: `/dev/root`, a `/dev/disk/by-uuid/...` symlink, or a
/// device-mapper node whose real storage is a partition of the target disk.
/// Comparing those strings against a target id misses exactly the case the
/// running-system gate exists for — the gate then reports the disk as unmounted
/// and plannable while it is the disk the system booted from.
///
/// Resolution therefore goes through the mount's `major:minor` from
/// `/proc/self/mountinfo`, then follows the slave chain so a filesystem on LVM or
/// md is attributed to the disk underneath it.
///
/// A running-system mount that cannot be resolved is refused rather than left
/// unattributed: not knowing which disk backs `/` means nothing proves the chosen
/// target is safe to overwrite.
pub(super) fn expand_mounts(
    root: &Path,
    mounts: &mut Vec<SystemMount>,
) -> Result<(), SystemInstallError> {
    let index = block_index(root);
    let mountinfo = read_mountinfo(root);
    let mut expanded = Vec::with_capacity(mounts.len());
    for mount in mounts.drain(..) {
        let names = resolve_mount_names(root, &index, &mountinfo, &mount);
        if names.is_empty() {
            if is_running_system_mount(&mount.mount_point) {
                return Err(SystemInstallError::Unsupported(format!(
                    "the running system is mounted at {} from {}, and that source could not be resolved to a block device, so the target cannot be proven safe to overwrite",
                    mount.mount_point, mount.device
                )));
            }
            expanded.push(mount);
            continue;
        }
        for name in names {
            expanded.push(SystemMount {
                device: format!("/dev/{name}"),
                mount_point: mount.mount_point.clone(),
                file_system: mount.file_system.clone(),
            });
        }
    }
    *mounts = expanded;
    Ok(())
}

/// Kernel block device names under the injected root.
fn block_names(root: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Ok(entries) = fs::read_dir(root.join("sys/class/block")) {
        for entry in entries.flatten() {
            names.insert(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names
}

/// `major:minor` to kernel name, as `/proc/self/mountinfo` spells the number.
fn block_index(root: &Path) -> BTreeMap<String, String> {
    let mut index = BTreeMap::new();
    if let Ok(entries) = fs::read_dir(root.join("sys/class/block")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(number) = read_attr(&entry.path().join("dev")) {
                index.insert(number, name);
            }
        }
    }
    index
}

/// Mount point to `major:minor`, from the kernel's own mount table.
fn read_mountinfo(root: &Path) -> BTreeMap<String, String> {
    let mut table = BTreeMap::new();
    let Ok(text) = fs::read_to_string(root.join("proc/self/mountinfo")) else {
        return table;
    };
    for line in text.lines() {
        // mountinfo: id parent major:minor root mount-point options [optional...]
        // The device number is field 3 and the mount point is field 5; the root
        // field between them is usually `/` for every mount, so reading it as the
        // mount point collapses the whole table onto one key.
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        table.insert(unescape_mount(fields[4]), fields[2].to_owned());
    }
    table
}

fn resolve_mount_names(
    root: &Path,
    index: &BTreeMap<String, String>,
    mountinfo: &BTreeMap<String, String>,
    mount: &SystemMount,
) -> Vec<String> {
    let known = block_names(root);
    let mut names = Vec::new();
    if let Some(number) = mountinfo.get(&mount.mount_point)
        && let Some(name) = index.get(number)
    {
        collect_slaves(root, name, &known, &mut names);
        return names;
    }
    // No usable mount table entry: a source that already is a kernel name, or a
    // fixture root. `/dev/root` and by-uuid symlinks need the table above.
    let name = mount.device.trim_start_matches("/dev/");
    if !name.is_empty() && known.contains(name) {
        collect_slaves(root, name, &known, &mut names);
    }
    names
}

/// Adds a device and every device underneath it, so a filesystem on device-mapper
/// or md is attributed to the disk that really stores it.
fn collect_slaves(root: &Path, name: &str, known: &BTreeSet<String>, names: &mut Vec<String>) {
    if names.iter().any(|existing| existing == name) {
        return;
    }
    names.push(name.to_owned());
    let mut children: Vec<String> =
        fs::read_dir(root.join("sys/class/block").join(name).join("slaves"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|child| known.contains(child))
            .collect();
    children.sort();
    for child in children {
        collect_slaves(root, &child, known, names);
    }
}

/// Whether a mount device (a device node or a kernel name) belongs to a target.
pub(super) fn resolves_to_target(device: &str, target: &SystemInstallTarget) -> bool {
    let name = device.trim_start_matches("/dev/");
    name == target.id || partition_number(target, name).is_some()
}

/// Kernel partitions of `mmcblk0`/`nvme0n1` are `pN`, of `sda` they are `N`.
fn partition_number(target: &SystemInstallTarget, partition: &str) -> Option<u32> {
    partition_number_of(&target.id, partition)
}

pub(super) fn partition_number_of(disk: &str, partition: &str) -> Option<u32> {
    let suffix = partition.strip_prefix(disk)?;
    // The kernel inserts `p` only when the disk name ends with a digit:
    // mmcblk0p1 and nvme0n1p1, but sda1.
    let digits = if disk.ends_with(|character: char| character.is_ascii_digit()) {
        suffix.strip_prefix('p')?
    } else {
        suffix
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

pub(super) fn longest_mount_prefix<'a>(
    mounts: &'a [SystemMount],
    path: &Path,
) -> Option<&'a SystemMount> {
    mounts
        .iter()
        .filter(|mount| path.starts_with(Path::new(&mount.mount_point)))
        .max_by_key(|mount| mount.mount_point.len())
}

pub(super) fn read_attr(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(super) fn parse_mounts(text: &str) -> Vec<SystemMount> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let device = fields.next()?;
            let mount_point = fields.next()?;
            let file_system = fields.next().unwrap_or_default();
            Some(SystemMount {
                device: unescape_mount(device),
                mount_point: unescape_mount(mount_point),
                file_system: file_system.to_owned(),
            })
        })
        .collect()
}

/// `/proc/mounts` escapes spaces and a few control characters as octal.
fn unescape_mount(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..index + 4]
                .iter()
                .all(|byte| (b'0'..=b'7').contains(byte))
        {
            let code = (bytes[index + 1] - b'0') * 64
                + (bytes[index + 2] - b'0') * 8
                + (bytes[index + 3] - b'0');
            out.push(code);
            index += 4;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BlockDeviceKind, SystemInstallRequest};
    use crate::system_install::test_support::*;
    use crate::system_install::{MIB, SystemInstallManager};

    /// The predicate behind "the image is stored on the device it would
    /// overwrite", tested directly because the mounted gate normally refuses
    /// that situation first.
    #[test]
    fn an_image_on_a_target_partition_is_detected() {
        let target = SystemInstallTarget {
            id: "sda".into(),
            path: "/dev/sda".into(),
            model: String::new(),
            size_bytes: 64 * MIB,
            kind: BlockDeviceKind::Ufs,
            removable: false,
            mounted: false,
            mount_points: Vec::new(),
            holds_running_system: false,
            partitions: Vec::new(),
        };
        let mounts = vec![
            SystemMount {
                device: "/dev/sda2".into(),
                mount_point: "/media/install".into(),
                file_system: "ext4".into(),
            },
            SystemMount {
                device: "/dev/other0".into(),
                mount_point: "/media/other".into(),
                file_system: "ext4".into(),
            },
        ];
        let mount = longest_mount_prefix(&mounts, Path::new("/media/install/radxa.img.xz"))
            .expect("the image has a mount");
        assert_eq!(mount.device, "/dev/sda2");
        assert!(resolves_to_target(&mount.device, &target));
        let elsewhere = longest_mount_prefix(&mounts, Path::new("/media/other/radxa.img"))
            .expect("the image has a mount");
        assert!(!resolves_to_target(&elsewhere.device, &target));
    }

    /// The regression from the finding: `/` reached through `/dev/root` or a
    /// by-uuid symlink must still be attributed to the disk that backs it. Before
    /// this, only the raw source string was compared, so the running system's own
    /// disk was reported as unmounted and produced a plan that would overwrite it.
    #[test]
    fn an_aliased_root_source_still_protects_the_running_disk() {
        let root = scratch("aliased-root");
        fs::create_dir_all(root.join("sys/class/block")).expect("create class directory");
        for (name, dev, size) in [("mmcblk1", "179:0", "62333952"), ("sda", "8:0", "131072")] {
            let class = root.join("sys/class/block").join(name);
            write_attr(&class.join("size"), &format!("{size}\n"));
            write_attr(&class.join("removable"), "0\n");
            write_attr(&class.join("dev"), &format!("{dev}\n"));
        }
        let partition = root.join("sys/class/block/mmcblk1p2");
        write_attr(&partition.join("dev"), "179:2\n");
        write_attr(&partition.join("partition"), "2\n");
        write_attr(&partition.join("start"), "540672\n");
        write_attr(&partition.join("size"), "819200\n");
        write_file(&root.join("dev/sda"), vec![0u8; 4 * MIB as usize]);
        // The source is not a kernel device name at all.
        write_file(&root.join("proc/mounts"), b"/dev/root / ext4 rw 0 0\n");
        write_file(
            &root.join("proc/self/mountinfo"),
            b"37 35 179:2 / / rw,relatime - ext4 /dev/root rw\n",
        );

        let manager = SystemInstallManager::new(Some(&root), false);
        let targets = manager.targets().expect("enumerate the fixture");
        let running = targets
            .iter()
            .find(|target| target.id == "mmcblk1")
            .expect("the running disk is enumerated");
        assert!(running.mounted, "{running:?}");
        assert!(running.holds_running_system, "{running:?}");

        let image = root.join("system.img");
        write_file(&image, mbr_image(1, 2 * MIB));
        let request = |target: &str| SystemInstallRequest {
            target_id: target.into(),
            image_path: Some(image.display().to_string()),
            image_id: None,
        };
        match manager.plan(&request("mmcblk1")) {
            Err(SystemInstallError::RunningSystem(path)) => {
                assert!(path.contains("mmcblk1"), "{path}");
            }
            other => panic!("the running disk must never be plannable: {other:?}"),
        }
        // A different disk is still writable, so the gate is not simply refusing.
        assert!(manager.plan(&request("sda")).is_ok());
    }

    /// Two mounts on two different disks must each be attributed to their own
    /// disk. The mount table lists the device number in field 3 and the mount
    /// point in field 5; reading the `root` field between them as the mount point
    /// keys every mount under `/`, so the last line wins and a disk that really
    /// hosts the running system is reported as free.
    #[test]
    fn mounts_on_different_disks_are_each_attributed() {
        let root = scratch("two-disks");
        fs::create_dir_all(root.join("sys/class/block")).expect("create class directory");
        for (name, dev, size) in [("sda", "8:0", "131072"), ("sdb", "8:16", "131072")] {
            let class = root.join("sys/class/block").join(name);
            write_attr(&class.join("size"), &format!("{size}\n"));
            write_attr(&class.join("removable"), "0\n");
            write_attr(&class.join("dev"), &format!("{dev}\n"));
        }
        for (name, dev, number) in [("sda2", "8:2", "2"), ("sdb2", "8:18", "2")] {
            let partition = root.join("sys/class/block").join(name);
            write_attr(&partition.join("dev"), &format!("{dev}\n"));
            write_attr(&partition.join("partition"), &format!("{number}\n"));
            write_attr(&partition.join("start"), "540672\n");
            write_attr(&partition.join("size"), "819200\n");
        }
        // `/` is named through a source that is not a kernel device name, so only
        // the mount table can attribute it; `/boot` is named directly.
        write_file(
            &root.join("proc/mounts"),
            b"/dev/root / ext4 rw 0 0\n/dev/sdb2 /boot vfat rw 0 0\n",
        );
        // `/` first and `/boot` second: a parser that keys both under `/` keeps the
        // boot disk and loses the root disk.
        write_file(
            &root.join("proc/self/mountinfo"),
            b"36 35 8:2 / / rw,relatime - ext4 /dev/root rw\n37 36 8:18 / /boot rw,relatime - vfat /dev/sdb2 rw\n",
        );

        let manager = SystemInstallManager::new(Some(&root), false);
        let targets = manager.targets().expect("enumerate the fixture");
        let by_id = |id: &str| {
            targets
                .iter()
                .find(|target| target.id == id)
                .unwrap_or_else(|| panic!("{id} is enumerated"))
        };
        let sda = by_id("sda");
        assert!(sda.holds_running_system, "sda hosts /: {sda:?}");
        assert!(sda.mounted, "sda has a mounted partition: {sda:?}");
        let sdb = by_id("sdb");
        assert!(sdb.holds_running_system, "sdb hosts /boot: {sdb:?}");
        assert_eq!(sdb.mount_points, vec!["/boot".to_owned()], "{sdb:?}");
        assert_eq!(sda.mount_points, vec!["/".to_owned()], "{sda:?}");

        // The root disk must not be plannable; the other one is only refused
        // because it also hosts a running-system mount.
        let image = root.join("system.img");
        write_file(&image, mbr_image(1, 2 * MIB));
        for target in ["sda", "sdb"] {
            let request = SystemInstallRequest {
                target_id: target.into(),
                image_path: Some(image.display().to_string()),
                image_id: None,
            };
            assert!(
                matches!(
                    manager.plan(&request),
                    Err(SystemInstallError::RunningSystem(_))
                ),
                "{target} hosts a running-system mount and must be refused"
            );
        }
    }

    /// A running-system mount that cannot be resolved must refuse rather than
    /// assume the target is a different disk.
    #[test]
    fn an_unresolvable_running_mount_refuses_every_target() {
        let root = scratch("unresolvable-root");
        fs::create_dir_all(root.join("sys/class/block/sda")).expect("create class directory");
        write_attr(&root.join("sys/class/block/sda/size"), "131072\n");
        write_attr(&root.join("sys/class/block/sda/dev"), "8:0\n");
        // No mount table entry, and the source is not a kernel device name.
        write_file(&root.join("proc/mounts"), b"/dev/root / ext4 rw 0 0\n");
        let manager = SystemInstallManager::new(Some(&root), false);
        match manager.targets() {
            Err(SystemInstallError::Unsupported(message)) => {
                assert!(message.contains("could not be resolved"), "{message}");
            }
            other => panic!("an unresolvable running root must refuse: {other:?}"),
        }
    }

    #[test]
    fn mounts_are_parsed_with_escapes_and_pseudo_devices_ignored() {
        let mounts = parse_mounts(
            "proc /proc proc rw 0 0\n/dev/sda1 /mnt/with\\040space ext4 rw 0 0\n/dev/mmcblk0p2 / ext4 rw 0 0\n",
        );
        assert_eq!(mounts.len(), 3);
        assert_eq!(mounts[1].mount_point, "/mnt/with space");
        let mut data = SystemInstallTarget {
            id: "sda".into(),
            path: "/dev/sda".into(),
            model: String::new(),
            size_bytes: 0,
            kind: BlockDeviceKind::Other,
            removable: false,
            mounted: false,
            mount_points: Vec::new(),
            holds_running_system: false,
            partitions: Vec::new(),
        };
        annotate_mounts(&mut data, &mounts);
        assert!(data.mounted);
        assert!(
            !data.holds_running_system,
            "a data mount is not the running system"
        );
        let mut root_target = SystemInstallTarget {
            id: "mmcblk0".into(),
            path: "/dev/mmcblk0".into(),
            model: String::new(),
            size_bytes: 0,
            kind: BlockDeviceKind::Emmc,
            removable: false,
            mounted: false,
            mount_points: Vec::new(),
            holds_running_system: false,
            partitions: Vec::new(),
        };
        annotate_mounts(&mut root_target, &mounts);
        assert!(root_target.holds_running_system);
        assert_eq!(root_target.mount_points, vec!["/"]);
    }

    #[test]
    fn name_matching_never_confuses_neighbouring_devices() {
        let target = SystemInstallTarget {
            id: "sda".into(),
            path: "/dev/sda".into(),
            model: String::new(),
            size_bytes: 0,
            kind: BlockDeviceKind::Other,
            removable: false,
            mounted: false,
            mount_points: Vec::new(),
            holds_running_system: false,
            partitions: Vec::new(),
        };
        assert!(resolves_to_target("/dev/sda", &target));
        assert!(resolves_to_target("/dev/sda1", &target));
        assert!(!resolves_to_target("/dev/sdab", &target));
        assert!(!resolves_to_target("/dev/sdb1", &target));
        assert_eq!(partition_number(&target, "sda12"), Some(12));
        assert_eq!(partition_number(&target, "sdap1"), None);
    }
}
