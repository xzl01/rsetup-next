//! Writing a whole system image onto a block device (onboard UFS, eMMC, SD or
//! NVMe), following the Radxa "install the system to the onboard storage"
//! procedure: boot from another medium, pick the target whole disk, verify the
//! image, write it, then re-read the partition table.
//!
//! # Gate invariants
//!
//! A wrong target destroys the running system and unrecoverable user data, so
//! the gates below are ordered from "cannot be overridden at all" to "must be
//! satisfied for every write":
//!
//! 1. **The device that hosts the running `/` or `/boot` is refused forever.**
//!    [`SystemInstallError::RunningSystem`] is raised while building a plan, so
//!    no plan token, typed acknowledgement, root privilege or retry can reach
//!    the write path. There is deliberately no override switch.
//! 2. **A target with a mounted partition is refused.** The error lists the
//!    mount points so the user can unmount first. This is re-checked while
//!    applying, so a plan made before a later automount cannot be replayed into
//!    a mounted target.
//! 3. **An acknowledgement is required and cannot be a boolean.** Callers pass
//!    the device name they typed; it must equal the plan's target id. A missing
//!    value is [`SystemInstallError::ConfirmationRequired`], a wrong one is
//!    [`SystemInstallError::AcknowledgementMismatch`]. Because the value is a
//!    string that must match observed device identity, `confirm: true` style
//!    booleans cannot satisfy it.
//! 4. **The plan token binds target identity, image identity and the expected
//!    acknowledgement**, is single-use inside the process that issued it, and
//!    expires [`PLAN_TTL`] after it was issued. Apply rebuilds the plan from the
//!    current device state and checks the presented token against it, so a
//!    swapped disk or a changed image file fails as
//!    [`SystemInstallError::StalePlan`].
//!
//! # Tokens: what is guaranteed, and by what
//!
//! A token is a **handle, not a credential**:
//!
//! * Every field — the state binding, the issuance time, the nonce — is covered
//!   by an HMAC-SHA256 signature made with a key that is created per process and
//!   never disclosed. Editing a field, or minting a token, fails verification, so
//!   the two shortcuts this used to allow (rewriting the nonce to make a consumed
//!   token look new, rewriting the time to restart the window) are gone.
//! * Single use and the [`PLAN_TTL`] window are decided by the **issuing
//!   process's record** of what it handed out, and consumption removes the entry
//!   in one step. A token another process minted is refused outright, whatever
//!   its fields say.
//! * Every surface holds one manager, so that process is the one that writes: the
//!   CLI, the TUI, the HTTP server and the desktop shell each issue and consume
//!   inside their own process. The privileged helper does the same: it
//!   **issues and consumes its own token** and never honours a caller's.
//! * A token therefore does not cross a process boundary as an authorization.
//!   [`SystemInstallManager::verify_preview_token`] lets a surface check that the
//!   preview the operator was shown still matches the state and is not old;
//!   because the caller writes the issuance time, **that window check is
//!   advisory** and this pair is a guard against mistakes and re-runs, not an
//!   adversarial boundary.
//!
//! What actually protects the device is everything else: the plan is rebuilt and
//! its binding must match, the hard gates below are re-evaluated, the device name
//! must be typed, and a real write needs administrator authorization.
//!
//! # Compressed images: what is checked, and where
//!
//! A `.xz` image is measured and its payload header is parsed while planning:
//!
//! * Its size is the sum of **every** stream in the file, because concatenating
//!   streams is valid and measuring only the last one would describe half an
//!   image.
//! * The three frames xz itself checksum — the stream header, the index and the
//!   stream footer — are verified, so a damaged file is refused before anything is
//!   written rather than after.
//! * A valid container is not evidence that the payload is a disk image: a
//!   compressed text file satisfies every container check. A bounded prefix of the
//!   decompressed content is therefore parsed as an MBR/GPT before a plan can
//!   exist, so a payload that would overwrite the target with something unbootable
//!   is refused instead of being written and only then found unverifiable. Damage
//!   inside that prefix is caught while planning; damage past it is still caught by
//!   the tool while writing.
//! * Only that prefix is decompressed while planning, so a multi-gigabyte image is
//!   not decompressed twice. The rest is checked by the tool: the decompressor's
//!   output is only treated as a complete stream once the child has exited
//!   successfully, and a non-zero exit is a hard failure that reports how many
//!   bytes were read and written, with the tool's own reason. Draining the output
//!   and never waiting for the child — which used to report a corrupt image as a
//!   successful install — is exactly what this rules out.
//! * The declared size is a bound, never the only stop: after writing it, one more
//!   read must be end-of-output, so a stream with more data fails as a conflict
//!   instead of being truncated.
//!
//! # No rollback
//!
//! Unlike the SPI flash path there is no backup and no rollback: a partial
//! write leaves the target partially overwritten on purpose. Failures report
//! how many bytes were already written in the error message, and the caller
//! must record that failed state rather than pretend the device was restored.
//!
//! # Testability and device safety
//!
//! Every device access goes through [`BlockDeviceBackend`] and every image read
//! through [`ImageOpener`]. [`SystemBlockBackend`] resolves sysfs, `/proc/mounts`
//! and the device node itself *under an injected root* (the `MmcManager`/
//! `NvmeManager` `probe_and_init(Some(root))` and `SpiFlashManager.root`
//! pattern), so a fixture root can never reach `/dev/*`, and the module is
//! driven end to end without root, without `/sys/class/block`, without `lsblk`
//! and without a real block device. Partition-table re-reads are platform-split:
//! a Linux `BLKRRPART` ioctl, and [`SystemInstallError::Unsupported`] elsewhere,
//! with the comparison itself living in the abstraction so it is testable on any
//! host.

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use crate::model::{
    BlockDeviceKind, SystemInstallImage, SystemInstallPartition, SystemInstallTarget,
};

mod backend_synth;
mod backend_sys;
mod crypto;
mod error;
mod image;
mod manager;
mod mounts;
mod opener;
#[cfg(test)]
mod test_support;
mod token;
mod xz;

pub use self::backend_synth::SyntheticBackend;
pub use self::backend_sys::SystemBlockBackend;
pub use self::error::SystemInstallError;
pub use self::manager::{SystemInstallManager, install_needs_privilege};
pub use self::opener::SystemImageOpener;

/// How much of an image is hashed to identify it. A whole multi-gigabyte image
/// is too slow to re-hash for every plan, while a prefix plus the exact size is
/// enough to notice a swapped or truncated file.
const IMAGE_PREFIX_BYTES: u64 = 1024 * 1024;

/// A plan token stops being valid this long after it was issued. The issuance
/// time travels inside the token, so this window is enforced by whichever process
/// applies it, not only by the one that planned it.
const PLAN_TTL: Duration = Duration::from_secs(600);

const SECTOR_BYTES: u64 = 512;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// One mount from `/proc/mounts`, reduced to what the gates need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemMount {
    pub device: String,
    pub mount_point: String,
    pub file_system: String,
}

/// Physical facts a backend reports. Mount and running-system state is derived
/// by the manager from [`BlockDeviceBackend::mounts`], so implementations only
/// describe hardware.
pub trait BlockDeviceBackend: Send + Sync {
    /// Whole disks only, with their current partitions.
    fn targets(&self) -> Result<Vec<SystemInstallTarget>, SystemInstallError>;

    /// Current mounts, used for the mounted and running-system gates and to
    /// refuse an image that is stored on the device being overwritten.
    fn mounts(&self) -> Result<Vec<SystemMount>, SystemInstallError>;

    /// Writes one chunk at a byte offset.
    fn write_chunk(
        &self,
        target: &SystemInstallTarget,
        offset: u64,
        chunk: &[u8],
    ) -> Result<(), SystemInstallError>;

    /// Flushes the target after the final chunk.
    fn sync_device(&self, _target: &SystemInstallTarget) -> Result<(), SystemInstallError> {
        Ok(())
    }

    /// Asks the kernel for the new partition table and reports it.
    fn reread_partitions(
        &self,
        target: &SystemInstallTarget,
    ) -> Result<Vec<SystemInstallPartition>, SystemInstallError>;

    /// Directory that holds the per-target write lock, or `None` when this backend
    /// cannot overwrite a real device. A real backend must answer `Some`: two plans
    /// naming the same target would otherwise be free to interleave their chunks.
    fn lock_root(&self) -> Option<PathBuf> {
        None
    }

    /// True when this backend describes a real target but never writes to it.
    ///
    /// A dry run must not be reported as a completed write: the caller asked for a
    /// preview, and a script reading the result has no other way to tell a counted
    /// byte from a written one.
    fn is_dry_run(&self) -> bool {
        false
    }
}

/// Opens the image as a byte stream, decompressing when the format says so.
pub trait ImageOpener: Send + Sync {
    fn open(&self, image: &SystemInstallImage) -> Result<Box<dyn Read + Send>, SystemInstallError>;
}

impl BlockDeviceKind {
    fn marker(self) -> &'static str {
        match self {
            BlockDeviceKind::Ufs => "ufs",
            BlockDeviceKind::Emmc => "emmc",
            BlockDeviceKind::Sd => "sd",
            BlockDeviceKind::Nvme => "nvme",
            BlockDeviceKind::Other => "other",
        }
    }
}
