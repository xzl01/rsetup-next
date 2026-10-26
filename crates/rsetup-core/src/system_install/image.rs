//! Image container facts read from the file itself: the partition table of a
//! raw image (MBR, or GPT behind a protective MBR), the identity of an image
//! file taken from an open handle, and the geometric comparison the
//! verification step uses to match the kernel's view against the plan.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crate::model::SystemInstallPartition;

use super::SECTOR_BYTES;
use super::error::SystemInstallError;

/// Compares partitions by geometry: names differ between an image and the
/// kernel's view, but the layout a system boots from does not.
/// Identity of an image file, from an open handle rather than a path.
///
/// The prefix digest only covers `prefix_bytes`, so this is what notices an edit
/// further into the file; device and inode notice a replacement.
pub(super) fn file_identity(metadata: &fs::Metadata) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}

pub(super) fn same_partition_layout(
    expected: &[SystemInstallPartition],
    observed: &[SystemInstallPartition],
) -> bool {
    if expected.len() != observed.len() {
        return false;
    }
    expected.iter().zip(observed).all(|(left, right)| {
        left.number == right.number
            && left.start_bytes == right.start_bytes
            && left.size_bytes == right.size_bytes
    })
}

/// Reads the partition table of a raw image: MBR, or GPT behind a protective
/// MBR.
pub(super) fn read_partition_table<R: Read + Seek>(
    file: &mut R,
) -> Result<(String, Vec<SystemInstallPartition>), SystemInstallError> {
    let mut sector = [0u8; 512];
    if file
        .seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut sector))
        .is_err()
    {
        return Err(SystemInstallError::InvalidInput(
            "the image is smaller than one sector and cannot hold a partition table".into(),
        ));
    }
    if sector[510] != 0x55 || sector[511] != 0xAA {
        return Err(SystemInstallError::InvalidInput(
            "the image has no MBR or GPT partition table; it is not a bootable system image".into(),
        ));
    }
    let mut primary = Vec::new();
    let mut protective = false;
    for index in 0..4 {
        let base = 446 + index * 16;
        let kind = sector[base + 4];
        let start =
            u32::from_le_bytes(sector[base + 8..base + 12].try_into().unwrap_or_default()) as u64;
        let sectors =
            u32::from_le_bytes(sector[base + 12..base + 16].try_into().unwrap_or_default()) as u64;
        if kind == 0xEE {
            protective = true;
            continue;
        }
        if kind == 0 && start == 0 && sectors == 0 {
            continue;
        }
        primary.push(SystemInstallPartition {
            name: format!("p{}", index + 1),
            number: index as u32 + 1,
            start_bytes: start * SECTOR_BYTES,
            size_bytes: sectors * SECTOR_BYTES,
            mount_point: None,
        });
    }
    if protective {
        return Ok(("gpt".into(), read_gpt(file)?));
    }
    if primary.is_empty() {
        return Err(SystemInstallError::InvalidInput(
            "the image MBR declares no partitions".into(),
        ));
    }
    Ok(("mbr".into(), primary))
}

fn read_gpt<R: Read + Seek>(
    file: &mut R,
) -> Result<Vec<SystemInstallPartition>, SystemInstallError> {
    let mut header = [0u8; 92];
    if file
        .seek(SeekFrom::Start(SECTOR_BYTES))
        .and_then(|_| file.read_exact(&mut header))
        .is_err()
    {
        return Err(SystemInstallError::InvalidInput(
            "the image is truncated inside its GPT header".into(),
        ));
    }
    if &header[0..8] != b"EFI PART" {
        return Err(SystemInstallError::InvalidInput(
            "the image declares a protective MBR but has no GPT header".into(),
        ));
    }
    let entries_lba = u64::from_le_bytes(header[72..80].try_into().unwrap_or_default());
    let count = u32::from_le_bytes(header[80..84].try_into().unwrap_or_default()) as usize;
    let entry_size = u32::from_le_bytes(header[84..88].try_into().unwrap_or_default()) as usize;
    if !(128..=4096).contains(&entry_size) || count == 0 || count > 4096 {
        return Err(SystemInstallError::InvalidInput(
            "the image GPT declares an implausible entry table".into(),
        ));
    }
    let readable = count.min(128);
    let mut table = vec![0u8; readable * entry_size];
    if file
        .seek(SeekFrom::Start(entries_lba * SECTOR_BYTES))
        .and_then(|_| file.read_exact(&mut table))
        .is_err()
    {
        return Err(SystemInstallError::InvalidInput(
            "the image is truncated inside its GPT entries".into(),
        ));
    }
    let mut partitions = Vec::new();
    for index in 0..readable {
        let base = index * entry_size;
        if table[base..base + 16].iter().all(|byte| *byte == 0) {
            continue;
        }
        let first = u64::from_le_bytes(table[base + 32..base + 40].try_into().unwrap_or_default());
        let last = u64::from_le_bytes(table[base + 40..base + 48].try_into().unwrap_or_default());
        if last < first {
            continue;
        }
        partitions.push(SystemInstallPartition {
            name: format!("p{}", index + 1),
            number: index as u32 + 1,
            start_bytes: first * SECTOR_BYTES,
            size_bytes: (last - first + 1) * SECTOR_BYTES,
            mount_point: None,
        });
    }
    if partitions.is_empty() {
        return Err(SystemInstallError::InvalidInput(
            "the image GPT declares no used partitions".into(),
        ));
    }
    Ok(partitions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstallImageFormat, SystemInstallImage, SystemInstallVerificationState};
    use crate::system_install::test_support::*;
    use crate::system_install::{ImageOpener, MIB, SystemInstallError, SystemInstallManager};
    use std::sync::Arc;

    #[test]
    fn identifies_gpt_and_xz_images() {
        let harness = fixture("formats", boot_mounts(), RereadBehavior::Matches);
        let gpt = harness.root.join("gpt.img");
        write_file(&gpt, gpt_image(&[(2048, 2048 + 4096), (8192, 8192 + 8192)]));
        let plan = harness
            .manager
            .plan(&install_request(&gpt))
            .expect("plan a GPT image");
        assert_eq!(plan.image.partition_table.as_deref(), Some("gpt"));
        assert_eq!(plan.image.partitions.len(), 2);
        assert_eq!(plan.image.partitions[0].start_bytes, 2048 * SECTOR_BYTES);
        assert_eq!(plan.image.partitions[0].size_bytes, 4097 * SECTOR_BYTES);

        let xz = harness.root.join("system.img.xz");
        write_file(&xz, XZ_FIXTURE);
        let plan = harness
            .manager
            .plan(&install_request(&xz))
            .expect("plan an xz image");
        assert_eq!(plan.image.format, InstallImageFormat::Xz);
        assert_eq!(
            plan.image.uncompressed_bytes, XZ_FIXTURE_BYTES,
            "the xz index declares it"
        );
        assert_eq!(plan.image.size_bytes, XZ_FIXTURE.len() as u64);
        assert_eq!(
            plan.image.partition_table.as_deref(),
            Some("mbr"),
            "the decompressed payload header is parsed while planning"
        );
        assert_eq!(plan.image.partitions.len(), 1);
        assert!(
            plan.warnings
                .contains(&"the_image_is_compressed_and_is_decompressed_while_writing".to_owned())
        );

        let corrupt = harness.root.join("corrupt.img.xz");
        write_file(&corrupt, &XZ_FIXTURE[..XZ_FIXTURE.len() - 4]);
        assert!(matches!(
            harness.manager.plan(&install_request(&corrupt)),
            Err(SystemInstallError::InvalidInput(_))
        ));

        let not_xz = harness.root.join("pretend.img.xz");
        write_file(&not_xz, mbr_image(1, 4 * MIB));
        let plan = harness
            .manager
            .plan(&install_request(&not_xz))
            .expect("a raw image keeps working despite the extension");
        assert_eq!(plan.image.format, InstallImageFormat::Raw);
    }

    /// A compressed image's header is parsed while planning, so its table is now
    /// compared like a raw image's. The opener returns a valid MBR and the fixture
    /// device re-reads a different table, so the result must be a mismatch — the
    /// "unsupported, nothing to compare" this used to report is what let a
    /// compressed non-image be written and only then found unverifiable.
    #[test]
    fn a_compressed_image_table_is_parsed_and_compared() {
        struct MbrOpener;
        impl ImageOpener for MbrOpener {
            fn open(
                &self,
                _image: &SystemInstallImage,
            ) -> Result<Box<dyn Read + Send>, SystemInstallError> {
                Ok(Box::new(std::io::Cursor::new(mbr_bytes(
                    XZ_FIXTURE_BYTES as usize,
                ))))
            }
        }
        let harness = fixture("xz-verify", boot_mounts(), RereadBehavior::Differs);
        let xz = harness.root.join("system.img.xz");
        write_file(&xz, XZ_FIXTURE);
        let manager =
            SystemInstallManager::with_backend(harness.backend.clone(), Arc::new(MbrOpener), false);
        let request = install_request(&xz);
        let plan = manager.plan(&request).expect("plan");
        assert_eq!(
            plan.image.partition_table.as_deref(),
            Some("mbr"),
            "the decompressed header was parsed while planning"
        );
        assert_eq!(plan.image.partitions.len(), 1);
        let result = manager
            .apply_live(&request, &plan.plan_token, "sda")
            .expect("write the compressed image");
        assert_eq!(
            result.verification.state,
            SystemInstallVerificationState::Mismatched,
            "{result:?}"
        );
        assert!(
            !result.verification.expected.is_empty(),
            "there is an expected layout to compare against"
        );
        let output = result.run.output.clone().unwrap_or_default();
        assert!(output.contains("DOES NOT match"), "{output}");
    }

    /// A compressed payload that is not a disk image must be refused while
    /// planning. A valid `.xz` container is not evidence of a bootable image: a
    /// compressed text file satisfies every container check.
    #[test]
    fn a_compressed_payload_that_is_not_a_disk_image_is_refused() {
        struct TextOpener;
        impl ImageOpener for TextOpener {
            fn open(
                &self,
                _image: &SystemInstallImage,
            ) -> Result<Box<dyn Read + Send>, SystemInstallError> {
                Ok(Box::new(std::io::Cursor::new(
                    b"this is not a disk image\n".to_vec(),
                )))
            }
        }
        let harness = fixture("xz-not-an-image", boot_mounts(), RereadBehavior::Matches);
        let xz = harness.root.join("notes.txt.xz");
        write_file(&xz, XZ_FIXTURE);
        let manager = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(TextOpener),
            false,
        );
        match manager.plan(&install_request(&xz)) {
            Err(SystemInstallError::InvalidInput(detail)) => {
                assert!(detail.contains("partition table"), "{detail}");
            }
            other => panic!("a compressed text file must never be plannable: {other:?}"),
        }
    }

    #[test]
    fn partition_layout_comparison_ignores_kernel_names() {
        let expected = vec![SystemInstallPartition {
            name: "p1".into(),
            number: 1,
            start_bytes: 2048 * SECTOR_BYTES,
            size_bytes: 8 * MIB,
            mount_point: None,
        }];
        let observed = vec![SystemInstallPartition {
            name: "sda1".into(),
            number: 1,
            start_bytes: 2048 * SECTOR_BYTES,
            size_bytes: 8 * MIB,
            mount_point: None,
        }];
        assert!(same_partition_layout(&expected, &observed));
        let moved = vec![SystemInstallPartition {
            name: "sda1".into(),
            number: 1,
            start_bytes: 4096 * SECTOR_BYTES,
            size_bytes: 8 * MIB,
            mount_point: None,
        }];
        assert!(!same_partition_layout(&expected, &moved));
        assert!(!same_partition_layout(&expected, &[]));
    }
}
