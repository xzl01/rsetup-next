//! The synthetic backend: labelled demo devices with counted writes, used for
//! `--demo` runs and as the default provider on non-Linux hosts. It is a
//! production backend, not a test double; it only never touches hardware.

use std::sync::Mutex;

use crate::model::{BlockDeviceKind, SystemInstallPartition, SystemInstallTarget};

use super::error::SystemInstallError;
use super::{BlockDeviceBackend, GIB, MIB, SystemMount};

/// Labelled demo devices: an onboard UFS module, an eMMC, the SD card the demo
/// system boots from, and an NVMe. Writes are counted, never stored.
#[derive(Default)]
pub struct SyntheticBackend {
    written: Mutex<u64>,
}

fn synthetic_target(
    id: &str,
    model: &str,
    size_bytes: u64,
    kind: BlockDeviceKind,
    removable: bool,
    partitions: &[(u32, u64, u64)],
) -> SystemInstallTarget {
    SystemInstallTarget {
        id: id.to_owned(),
        path: format!("/dev/{id}"),
        model: model.to_owned(),
        size_bytes,
        kind,
        removable,
        mounted: false,
        mount_points: Vec::new(),
        holds_running_system: false,
        partitions: partitions
            .iter()
            .map(|(number, start, size)| SystemInstallPartition {
                name: if id.starts_with("sd") {
                    format!("{id}{number}")
                } else {
                    format!("{id}p{number}")
                },
                number: *number,
                start_bytes: *start,
                size_bytes: *size,
                mount_point: None,
            })
            .collect(),
    }
}

impl BlockDeviceBackend for SyntheticBackend {
    fn targets(&self) -> Result<Vec<SystemInstallTarget>, SystemInstallError> {
        Ok(vec![
            synthetic_target(
                "sda",
                "RADXA UFS 128GB",
                128 * GIB,
                BlockDeviceKind::Ufs,
                false,
                &[
                    (1, MIB, 128 * MIB),
                    (2, 129 * MIB, 2 * GIB),
                    (3, 2 * GIB + 129 * MIB, 6 * GIB),
                ],
            ),
            synthetic_target(
                "mmcblk0",
                "AJTD4R eMMC",
                58 * GIB,
                BlockDeviceKind::Emmc,
                false,
                &[(1, 16 * MIB, 512 * MIB), (2, 528 * MIB, 57 * GIB)],
            ),
            synthetic_target(
                "mmcblk1",
                "SD 32GB",
                29 * GIB,
                BlockDeviceKind::Sd,
                true,
                &[(1, 16 * MIB, 256 * MIB), (2, 272 * MIB, 28 * GIB)],
            ),
            synthetic_target(
                "nvme0n1",
                "Radxa M.2 NVMe 256GB",
                238 * GIB,
                BlockDeviceKind::Nvme,
                false,
                &[(1, MIB, 238 * GIB)],
            ),
        ])
    }

    fn mounts(&self) -> Result<Vec<SystemMount>, SystemInstallError> {
        Ok(vec![
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
        ])
    }

    fn write_chunk(
        &self,
        _target: &SystemInstallTarget,
        _offset: u64,
        chunk: &[u8],
    ) -> Result<(), SystemInstallError> {
        let mut written = self.written.lock().expect("synthetic write lock");
        *written += chunk.len() as u64;
        Ok(())
    }

    fn reread_partitions(
        &self,
        _target: &SystemInstallTarget,
    ) -> Result<Vec<SystemInstallPartition>, SystemInstallError> {
        // The demo provider cannot inspect the written bytes, which is exactly
        // what a host without BLKRRPART reports.
        Err(SystemInstallError::Unsupported(
            "the demo provider does not re-read partition tables".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SystemInstallRequest;
    use crate::system_install::test_support::*;
    use crate::system_install::{
        MIB, SystemInstallError, SystemInstallManager, SystemInstallVerificationState,
    };

    #[test]
    fn the_demo_provider_is_labelled_and_never_writes() {
        let manager = SystemInstallManager::new(None, true);
        let targets = manager.targets().expect("demo targets");
        let ufs = targets
            .iter()
            .find(|target| target.id == "sda")
            .expect("the demo UFS module is present");
        assert_eq!(ufs.kind, BlockDeviceKind::Ufs);
        assert_eq!(ufs.model, "RADXA UFS 128GB");
        let boot = targets
            .iter()
            .find(|target| target.id == "mmcblk1")
            .expect("the demo SD card is present");
        assert!(boot.holds_running_system);
        assert_eq!(boot.mount_points, vec!["/", "/boot"]);
        let demo_root = scratch("demo");
        let image = demo_root.join("system.img");
        write_file(&image, mbr_image(1, 4 * MIB));
        assert!(matches!(
            manager.plan(&SystemInstallRequest {
                target_id: "mmcblk1".into(),
                image_path: Some(image.display().to_string()),
                image_id: None,
            }),
            Err(SystemInstallError::RunningSystem(_))
        ));
        let request = SystemInstallRequest {
            target_id: "sda".into(),
            image_path: Some(image.display().to_string()),
            image_id: None,
        };
        let plan = manager.plan(&request).expect("plan the demo UFS target");
        assert!(plan.synthetic);
        let result = manager
            .apply_live(&request, &plan.plan_token, "sda")
            .expect("demo write");
        assert_eq!(result.bytes_written, plan.image.uncompressed_bytes);
        assert!(result.run.summary.contains("no device was written"));
        assert!(result.run.synthetic);
        assert_eq!(
            result.verification.state,
            SystemInstallVerificationState::Unsupported
        );
    }
}
