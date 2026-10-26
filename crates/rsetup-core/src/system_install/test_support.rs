//! Test-only fixtures and helpers shared by the `system_install` test
//! modules: scratch directories under `target/`, MBR/GPT/xz image builders
//! and the fixture backends.

use super::*;

/// Scratch space under the workspace `target/` directory, so tests only ever
/// use ordinary files and never a real block device.
pub(super) fn scratch(name: &str) -> Scratch {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/system-install-tests")
        .join(format!("{name}-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).expect("create scratch directory");
    Scratch(root)
}

/// Owns one test's fixture directory and removes it on drop, so the suite
/// cannot accumulate image-sized files in `target/` (also on a panic).
pub(super) struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn write_file(path: &Path, bytes: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, bytes.as_ref()).expect("write fixture file");
}

pub(super) fn write_attr(path: &Path, value: &str) {
    write_file(path, value.as_bytes());
}

/// A raw image with an MBR and `count` partitions of `partition_bytes`.
pub(super) fn mbr_image(count: usize, partition_bytes: u64) -> Vec<u8> {
    let mut image = vec![0u8; (partition_bytes * count as u64 + 2 * MIB) as usize];
    image[510] = 0x55;
    image[511] = 0xAA;
    for index in 0..count {
        let base = 446 + index * 16;
        image[base + 4] = 0x83;
        let start = 2048 + index as u32 * (partition_bytes / SECTOR_BYTES) as u32;
        image[base + 8..base + 12].copy_from_slice(&start.to_le_bytes());
        image[base + 12..base + 16]
            .copy_from_slice(&((partition_bytes / SECTOR_BYTES) as u32).to_le_bytes());
    }
    image
}

/// A valid MBR disk image of exactly `len` bytes, for openers that stand in
/// for a decompressor. The payload header has to parse now that a compressed
/// image's table is read while planning.
pub(super) fn mbr_bytes(len: usize) -> Vec<u8> {
    assert!(len >= SECTOR_BYTES as usize, "an MBR needs one sector");
    let mut image = vec![0u8; len];
    image[510] = 0x55;
    image[511] = 0xAA;
    image[446 + 4] = 0x83;
    image[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
    image[446 + 12..446 + 16].copy_from_slice(&1024u32.to_le_bytes());
    image
}

/// The MBR the fixture image declares, as the verification compares it.
pub(super) fn mbr_partitions(partition_bytes: u64) -> Vec<SystemInstallPartition> {
    (0..2)
        .map(|index| SystemInstallPartition {
            name: format!("p{}", index + 1),
            number: index + 1,
            start_bytes: (2048 + index * (partition_bytes / SECTOR_BYTES) as u32) as u64
                * SECTOR_BYTES,
            size_bytes: partition_bytes,
            mount_point: None,
        })
        .collect()
}

pub(super) fn gpt_image(entries: &[(u64, u64)]) -> Vec<u8> {
    let mut image = vec![0u8; 4 * MIB as usize];
    image[510] = 0x55;
    image[511] = 0xAA;
    image[446 + 4] = 0xEE;
    image[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    image[446 + 12..446 + 16].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    let header = 512;
    image[header..header + 8].copy_from_slice(b"EFI PART");
    image[header + 72..header + 80].copy_from_slice(&2u64.to_le_bytes());
    image[header + 80..header + 84].copy_from_slice(&(entries.len() as u32).to_le_bytes());
    image[header + 84..header + 88].copy_from_slice(&128u32.to_le_bytes());
    let table = 1024;
    for (index, (first, last)) in entries.iter().enumerate() {
        let base = table + index * 128;
        image[base] = 0x11;
        image[base + 32..base + 40].copy_from_slice(&first.to_le_bytes());
        image[base + 40..base + 48].copy_from_slice(&last.to_le_bytes());
    }
    image
}

/// A real `.xz` stream wrapping a 512 byte MBR disk image with one 0x83
/// partition (LBA 2048, 1024 sectors), i.e. [`XZ_FIXTURE_BYTES`]
/// uncompressed. Pinned here so the parser is tested without a compression
/// dependency and without running a tool.
///
/// The payload is a disk image on purpose: a compressed text file is a valid
/// `.xz` container, so a fixture that decompressed to prose would no longer
/// be plannable now that the payload header is parsed.
pub(super) const XZ_FIXTURE: &[u8] = &[
    0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00, 0x00, 0x04, 0xE6, 0xD6, 0xB4, 0x46, 0x04, 0xC0, 0x1B, 0x80,
    0x04, 0x21, 0x01, 0x1C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xB1, 0x1F, 0x34, 0xD4,
    0xE0, 0x01, 0xFF, 0x00, 0x13, 0x5D, 0x00, 0x00, 0x6F, 0xFD, 0xFC, 0x9F, 0x2F, 0x12, 0x9F, 0xA0,
    0x86, 0x59, 0x2A, 0xA2, 0x11, 0x28, 0x99, 0x85, 0x40, 0x00, 0x00, 0x00, 0x58, 0x96, 0x4C, 0x17,
    0xCF, 0xA3, 0x7F, 0xE2, 0x00, 0x01, 0x37, 0x80, 0x04, 0x00, 0x00, 0x00, 0x0D, 0xC4, 0xAE, 0xE4,
    0xB1, 0xC4, 0x67, 0xFB, 0x02, 0x00, 0x00, 0x00, 0x00, 0x04, 0x59, 0x5A,
];

/// Uncompressed length of [`XZ_FIXTURE`].
pub(super) const XZ_FIXTURE_BYTES: u64 = 512;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum RereadBehavior {
    Matches,
    Differs,
    Unsupported,
    /// The kernel refuses the re-read itself (EINVAL on the board).
    Fails,
}

/// Fixture backend: writes into an ordinary file under `target/`, and can be
/// told to fail part way or to disagree about the resulting table.
pub(super) struct FixtureBackend {
    pub(super) targets: Mutex<Vec<SystemInstallTarget>>,
    pub(super) mounts: Vec<SystemMount>,
    pub(super) device: PathBuf,
    pub(super) fail_after: Mutex<Option<u64>>,
    pub(super) behavior: Mutex<RereadBehavior>,
    pub(super) chunks: Mutex<Vec<usize>>,
    pub(super) expected: Vec<SystemInstallPartition>,
}

impl BlockDeviceBackend for FixtureBackend {
    fn targets(&self) -> Result<Vec<SystemInstallTarget>, SystemInstallError> {
        Ok(self.targets.lock().expect("targets lock").clone())
    }

    fn lock_root(&self) -> Option<PathBuf> {
        // The device is <root>/device.img, so its parent is this test's own
        // root and the lock lands in <root>/run/lock. Deriving one level too
        // far would put every test on the shared parent directory.
        self.device.parent().map(Path::to_path_buf)
    }

    fn mounts(&self) -> Result<Vec<SystemMount>, SystemInstallError> {
        Ok(self.mounts.clone())
    }

    fn write_chunk(
        &self,
        _target: &SystemInstallTarget,
        offset: u64,
        chunk: &[u8],
    ) -> Result<(), SystemInstallError> {
        let mut written = self.chunks.lock().expect("chunk lock");
        let total: u64 = written.iter().map(|size| *size as u64).sum::<u64>() + chunk.len() as u64;
        // The guard of `fail_after` stays held for the whole statement,
        // exactly as it did with the nested form.
        if let Some(fail_after) = *self.fail_after.lock().expect("fail lock")
            && total > fail_after
        {
            return Err(SystemInstallError::Io("simulated device failure".into()));
        }
        written.push(chunk.len());
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.device)
            .expect("open fixture device");
        file.seek(SeekFrom::Start(offset))
            .expect("seek fixture device");
        file.write_all(chunk).expect("write fixture device");
        Ok(())
    }

    fn reread_partitions(
        &self,
        target: &SystemInstallTarget,
    ) -> Result<Vec<SystemInstallPartition>, SystemInstallError> {
        match *self.behavior.lock().expect("behavior lock") {
            RereadBehavior::Unsupported => Err(SystemInstallError::Unsupported(
                "the fixture host does not re-read partition tables".into(),
            )),
            RereadBehavior::Fails => Err(SystemInstallError::Io(
                "the kernel did not re-read /dev/sda: Invalid argument (os error 22)".into(),
            )),
            RereadBehavior::Differs => Ok(vec![SystemInstallPartition {
                name: "p1".into(),
                number: 1,
                start_bytes: 4096,
                size_bytes: target.size_bytes / 4,
                mount_point: None,
            }]),
            RereadBehavior::Matches => Ok(self.expected.clone()),
        }
    }
}

pub(super) struct Fixture {
    pub(super) root: Scratch,
    pub(super) device: PathBuf,
    pub(super) image: PathBuf,
    pub(super) backend: Arc<FixtureBackend>,
    pub(super) manager: SystemInstallManager,
}

pub(super) fn fixture(name: &str, mounts: Vec<SystemMount>, reread: RereadBehavior) -> Fixture {
    let root = scratch(name);
    let device = root.join("device.img");
    write_file(&device, vec![0u8; 24 * MIB as usize]);
    let image = root.join("system.img");
    write_file(&image, mbr_image(2, 8 * MIB));
    let backend = Arc::new(FixtureBackend {
        targets: Mutex::new(vec![SystemInstallTarget {
            id: "sda".into(),
            path: "/dev/sda".into(),
            model: "FIXTURE UFS 128GB".into(),
            size_bytes: 64 * MIB,
            kind: BlockDeviceKind::Ufs,
            removable: false,
            mounted: false,
            mount_points: Vec::new(),
            holds_running_system: false,
            partitions: vec![
                SystemInstallPartition {
                    name: "sda1".into(),
                    number: 1,
                    start_bytes: MIB,
                    size_bytes: 4 * MIB,
                    mount_point: None,
                },
                SystemInstallPartition {
                    name: "sda2".into(),
                    number: 2,
                    start_bytes: 5 * MIB,
                    size_bytes: 8 * MIB,
                    mount_point: None,
                },
            ],
        }]),
        mounts,
        device: device.clone(),
        fail_after: Mutex::new(None),
        behavior: Mutex::new(reread),
        chunks: Mutex::new(Vec::new()),
        expected: mbr_partitions(8 * MIB),
    });
    let manager =
        SystemInstallManager::with_backend(backend.clone(), Arc::new(SystemImageOpener), false);
    Fixture {
        root,
        device,
        image,
        backend,
        manager,
    }
}

pub(super) fn install_request(image: &Path) -> SystemInstallRequest {
    SystemInstallRequest {
        target_id: "sda".into(),
        image_path: Some(image.display().to_string()),
        image_id: None,
    }
}

pub(super) fn boot_mounts() -> Vec<SystemMount> {
    vec![
        SystemMount {
            device: "/dev/mmcblk1p2".into(),
            mount_point: "/".into(),
            file_system: "ext4".into(),
        },
        SystemMount {
            device: "/dev/mmcblk1p1".into(),
            mount_point: "/boot".into(),
            file_system: "vfat".into(),
        },
    ]
}

/// Whether the real tool is available; the subprocess tests skip without it
/// (the `sh` based tests below cover the same status logic on every host).
pub(super) fn xz_available() -> bool {
    Command::new("xz")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub(super) fn flip(bytes: &[u8], offset: usize) -> Vec<u8> {
    let mut mutated = bytes.to_vec();
    mutated[offset] ^= 0xFF;
    mutated
}

/// One `.xz` stream whose index holds four block records (made with
/// `xz --block-size=256` over a 1024 byte MBR image, so the per-record
/// summing is exercised too). The last stream of a concatenation is what a
/// last-stream-only parser reports, which is the truncation this guards.
pub(super) const XZ_MULTI_BLOCK_FIXTURE: &[u8] = &[
    0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00, 0x00, 0x04, 0xE6, 0xD6, 0xB4, 0x46, 0x03, 0xC0, 0x0E, 0x80,
    0x02, 0x21, 0x01, 0x1C, 0x00, 0x00, 0x00, 0x00, 0x60, 0x94, 0xDD, 0x90, 0xE0, 0x00, 0xFF, 0x00,
    0x06, 0x5D, 0x00, 0x00, 0x6F, 0xD8, 0x40, 0x98, 0x00, 0x00, 0x00, 0x00, 0xDC, 0xCE, 0x17, 0xE2,
    0x4C, 0x2C, 0xD5, 0xD0, 0x03, 0xC0, 0x19, 0x80, 0x02, 0x21, 0x01, 0x1C, 0x00, 0x00, 0x00, 0x00,
    0xF1, 0x0E, 0xAF, 0x55, 0xE0, 0x00, 0xFF, 0x00, 0x11, 0x5D, 0x00, 0x00, 0x6F, 0x5C, 0xC6, 0x0E,
    0x32, 0x06, 0xA7, 0xE0, 0x80, 0x91, 0xFA, 0x7A, 0x74, 0x4A, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xE8, 0xA4, 0x00, 0x59, 0xA1, 0x64, 0x38, 0x5B, 0x03, 0xC0, 0x0E, 0x80, 0x02, 0x21, 0x01, 0x1C,
    0x00, 0x00, 0x00, 0x00, 0x60, 0x94, 0xDD, 0x90, 0xE0, 0x00, 0xFF, 0x00, 0x06, 0x5D, 0x00, 0x00,
    0x6F, 0xD8, 0x40, 0x98, 0x00, 0x00, 0x00, 0x00, 0xDC, 0xCE, 0x17, 0xE2, 0x4C, 0x2C, 0xD5, 0xD0,
    0x03, 0xC0, 0x0E, 0x80, 0x02, 0x21, 0x01, 0x1C, 0x00, 0x00, 0x00, 0x00, 0x60, 0x94, 0xDD, 0x90,
    0xE0, 0x00, 0xFF, 0x00, 0x06, 0x5D, 0x00, 0x00, 0x6F, 0xD8, 0x40, 0x98, 0x00, 0x00, 0x00, 0x00,
    0xDC, 0xCE, 0x17, 0xE2, 0x4C, 0x2C, 0xD5, 0xD0, 0x00, 0x04, 0x26, 0x80, 0x02, 0x31, 0x80, 0x02,
    0x26, 0x80, 0x02, 0x26, 0x80, 0x02, 0x00, 0x00, 0x52, 0x8B, 0xBC, 0x64, 0xAC, 0x27, 0x3E, 0x2D,
    0x04, 0x00, 0x00, 0x00, 0x00, 0x04, 0x59, 0x5A,
];
