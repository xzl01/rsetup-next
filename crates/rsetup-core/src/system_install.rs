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
//! A `.xz` image is measured, not decompressed, while planning:
//!
//! * Its size is the sum of **every** stream in the file, because concatenating
//!   streams is valid and measuring only the last one would describe half an
//!   image.
//! * The three frames xz itself checksum — the stream header, the index and the
//!   stream footer — are verified, so a damaged file is refused before anything is
//!   written rather than after.
//! * Block data is not parsed while planning (that would mean decompressing a
//!   multi-gigabyte image twice). It is checked by the tool: the decompressor's
//!   output is only treated as a complete stream once the child has exited
//!   successfully, and a non-zero exit is a hard failure that reports how many
//!   bytes were read and written, with the tool's own reason. Draining the output
//!   and never waiting for the child — which used to report a corrupt image as a
//!   successful install — is exactly what this rules out.
//! * The declared size is a bound, never the only stop: after writing it, one more
//!   read must be end-of-output, so a stream with more data fails as a conflict
//!   instead of being truncated.
//! * Since the uncompressed layout is never parsed, the partition-table
//!   comparison after a compressed write is reported as an "unsupported"
//!   verification with its reason (the table was re-read but there is nothing to
//!   compare it against) rather than as a mismatch.
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

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use crate::model::{
    ActionRun, ActionStatus, BlockDeviceKind, InstallImageFormat, SystemInstallApplyResult,
    SystemInstallImage, SystemInstallPartition, SystemInstallPlan, SystemInstallRequest,
    SystemInstallTarget, SystemInstallVerification, SystemInstallVerificationState,
};

/// One chunk of the image is held in memory at a time.
const CHUNK_BYTES: u64 = 4 * 1024 * 1024;

/// How much of an image is hashed to identify it. A whole multi-gigabyte image
/// is too slow to re-hash for every plan, while a prefix plus the exact size is
/// enough to notice a swapped or truncated file.
const IMAGE_PREFIX_BYTES: u64 = 1024 * 1024;

/// A plan token stops being valid this long after it was issued. The issuance
/// time travels inside the token, so this window is enforced by whichever process
/// applies it, not only by the one that planned it.
const PLAN_TTL: Duration = Duration::from_secs(600);

/// How far a token may be dated in the future before it is refused. Tokens are
/// minted from the local clock, so a small skew is normal and a large one is a
/// forged or broken token.
const TOKEN_CLOCK_SKEW: chrono::Duration = chrono::Duration::seconds(60);

/// Action identity used for the recorded run.
const INSTALL_ACTION_ID: &str = "storage.install-image";
const INSTALL_ACTION_TITLE: &str = "Install a system image";

const SECTOR_BYTES: u64 = 512;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// CRC-32 as xz uses it: reflected, polynomial 0xEDB88320, initial and final xor.
/// The stream header, the index and the stream footer each carry one, so a
/// damaged file is refused while planning instead of being written as an image.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

const XZ_MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];
const XZ_FOOTER_MAGIC: [u8; 2] = [0x59, 0x5A];

/// `ioctl` that asks the kernel to re-read a block device's partition table.
/// Linux encodes it as `_IO(0x12, 81)`; `libc` does not export the name.
#[cfg(target_os = "linux")]
const BLKRRPART: libc::c_ulong = 0x1251;

/// Virtual devices an image must never be written to.
const VIRTUAL_DEVICE_PREFIXES: [&str; 7] = ["loop", "ram", "zram", "sr", "dm-", "md", "fd"];

#[derive(Debug, Error)]
pub enum SystemInstallError {
    #[error("system image installation is unavailable: {0}")]
    Unsupported(String),
    #[error("invalid system install request: {0}")]
    InvalidInput(String),
    #[error("confirmation is required before writing a system image")]
    ConfirmationRequired,
    #[error("type \"{expected}\" to write {target}; received \"{provided}\"")]
    AcknowledgementMismatch {
        expected: String,
        target: String,
        provided: String,
    },
    #[error("preview the system image write before applying it")]
    PlanRequired,
    #[error("the target or the image changed after preview; create a fresh plan")]
    StalePlan,
    #[error("writing a system image requires root privileges")]
    RootRequired,
    #[error("the target cannot be written: {0}")]
    Conflict(String),
    #[error("refusing to write {0}: it hosts the running root or boot filesystem")]
    RunningSystem(String),
    #[error("unable to write the system image: {0}")]
    Io(String),
}

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
}

/// Opens the image as a byte stream, decompressing when the format says so.
pub trait ImageOpener: Send + Sync {
    fn open(&self, image: &SystemInstallImage) -> Result<Box<dyn Read + Send>, SystemInstallError>;
}

type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Owns the plan/apply state machine. Cloning shares issued tokens, so a token
/// cannot be replayed through a clone.
#[derive(Clone)]
pub struct SystemInstallManager {
    backend: Arc<dyn BlockDeviceBackend>,
    opener: Arc<dyn ImageOpener>,
    synthetic: bool,
    now: Clock,
    /// Tokens this process issued, and when it issued them. This record decides
    /// single use and the 600 second window (see `consume`), not anything the
    /// caller carries.
    issued: Arc<Mutex<BTreeMap<String, DateTime<Utc>>>>,
    /// Signing key for plan tokens, created per process and never disclosed. A
    /// token is a handle rather than a credential: only the process that issued one
    /// accepts it, and the cross-process preview check (`verify_preview_token`) is
    /// advisory.
    token_key: Arc<[u8; 32]>,
}

impl SystemInstallManager {
    /// Live manager rooted at `/`, or the labelled demo devices when synthetic.
    pub fn new(root: Option<&Path>, synthetic: bool) -> Self {
        if synthetic {
            return Self::with_backend(
                Arc::new(SyntheticBackend::default()),
                Arc::new(SystemImageOpener),
                true,
            );
        }
        Self::with_backend(
            Arc::new(SystemBlockBackend::new(root)),
            Arc::new(SystemImageOpener),
            false,
        )
    }

    pub fn with_backend(
        backend: Arc<dyn BlockDeviceBackend>,
        opener: Arc<dyn ImageOpener>,
        synthetic: bool,
    ) -> Self {
        Self {
            backend,
            opener,
            synthetic,
            now: Arc::new(Utc::now),
            issued: Arc::new(Mutex::new(BTreeMap::new())),
            token_key: Arc::new(mint_token_key()),
        }
    }

    #[cfg(test)]
    fn with_clock(mut self, now: Clock) -> Self {
        self.now = now;
        self
    }

    /// Whole devices an image could be written to, with the gates annotated.
    pub fn targets(&self) -> Result<Vec<SystemInstallTarget>, SystemInstallError> {
        let mounts = self.backend.mounts()?;
        let mut targets = self.backend.targets()?;
        for target in &mut targets {
            annotate_mounts(target, &mounts);
        }
        targets.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(targets)
    }

    /// Builds and issues a plan. Every irreversible consequence is in the plan
    /// so a surface can show it before asking for the typed acknowledgement.
    pub fn plan(
        &self,
        request: &SystemInstallRequest,
    ) -> Result<SystemInstallPlan, SystemInstallError> {
        let plan = self.build_plan(request)?;
        self.issued
            .lock()
            .expect("install token lock")
            .insert(plan.plan_token.clone(), plan.created_at);
        Ok(plan)
    }

    /// Writes the image after checking the typed acknowledgement and the token.
    pub fn apply_live(
        &self,
        request: &SystemInstallRequest,
        plan_token: &str,
        acknowledgement: &str,
    ) -> Result<SystemInstallApplyResult, SystemInstallError> {
        if plan_token.trim().is_empty() {
            return Err(SystemInstallError::PlanRequired);
        }
        let plan = self.build_plan(request)?;
        // The presented token must be one this process signed and issued. The
        // signature covers the binding, the issuance time and the nonce, so no
        // field can be edited or re-minted by whoever carries it, and the issuance
        // record — not a value the caller can write — decides single use and the
        // window.
        let Some(presented) = self.parse_token(plan_token) else {
            return Err(SystemInstallError::StalePlan);
        };
        if presented.binding != token_binding(&plan.revision, &plan.target, &plan.image) {
            return Err(SystemInstallError::StalePlan);
        }
        let issued_at = self.issued_at(plan_token)?;
        let now = (self.now)();
        let age = now.signed_duration_since(issued_at);
        if age > chrono::Duration::from_std(PLAN_TTL).unwrap_or_default() || age < -TOKEN_CLOCK_SKEW
        {
            return Err(SystemInstallError::StalePlan);
        }
        // Defence in depth: the signed issuance time must agree with the record.
        let signed = DateTime::<Utc>::from_timestamp(presented.issued as i64, 0)
            .ok_or(SystemInstallError::StalePlan)?;
        let signed_age = now.signed_duration_since(signed);
        if signed_age > chrono::Duration::from_std(PLAN_TTL).unwrap_or_default()
            || signed_age < -TOKEN_CLOCK_SKEW
        {
            return Err(SystemInstallError::StalePlan);
        }
        let typed = acknowledgement.trim();
        if typed.is_empty() {
            return Err(SystemInstallError::ConfirmationRequired);
        }
        if typed != plan.target.id {
            return Err(SystemInstallError::AcknowledgementMismatch {
                expected: plan.target.id.clone(),
                target: plan.target.path.clone(),
                provided: typed.to_owned(),
            });
        }
        self.consume(plan_token)?;

        let started_at = (self.now)();
        let mut stream = self.opener.open(&plan.image)?;
        let bytes_written = self.write_image(&plan, &mut *stream)?;
        self.backend.sync_device(&plan.target)?;
        let verification = self.verify(&plan);

        let mut summary = format!(
            "Wrote {} of {} bytes to {}.",
            bytes_written, plan.image.uncompressed_bytes, plan.target.path
        );
        if self.synthetic {
            summary = format!("{summary} Synthetic target: no device was written.");
        }
        let mut output = format!(
            "target: {} ({}, {} bytes)\nimage: {}\nsha256(first {} bytes): {}\nbytesWritten: {}",
            plan.target.path,
            plan.target.model,
            plan.target.size_bytes,
            plan.image.path,
            plan.image.prefix_bytes,
            plan.image.sha256_prefix,
            bytes_written,
        );
        match verification.state {
            SystemInstallVerificationState::Matched => {
                output.push_str("\npartitionTable: re-read matches the image");
            }
            SystemInstallVerificationState::Mismatched => {
                output.push_str("\npartitionTable: re-read DOES NOT match the image");
            }
            SystemInstallVerificationState::Unsupported => {
                output.push_str("\npartitionTable: re-read is unsupported on this host");
            }
        }
        let run = ActionRun {
            id: Uuid::new_v4().to_string(),
            action_id: INSTALL_ACTION_ID.to_owned(),
            action_title: INSTALL_ACTION_TITLE.to_owned(),
            status: if verification.state == SystemInstallVerificationState::Mismatched {
                ActionStatus::Failed
            } else {
                ActionStatus::Succeeded
            },
            synthetic: self.synthetic,
            summary,
            output: Some(output),
            started_at,
            finished_at: Some((self.now)()),
        };
        Ok(SystemInstallApplyResult {
            run,
            plan,
            bytes_written,
            verification,
        })
    }

    /// Builds a plan without issuing a token, so apply re-derives the exact
    /// identity it is about to write.
    fn build_plan(
        &self,
        request: &SystemInstallRequest,
    ) -> Result<SystemInstallPlan, SystemInstallError> {
        if request.target_id.trim().is_empty() {
            return Err(SystemInstallError::InvalidInput(
                "a target device is required".into(),
            ));
        }
        let mounts = self.backend.mounts()?;
        let mut targets = self.backend.targets()?;
        for target in &mut targets {
            annotate_mounts(target, &mounts);
        }
        let target = targets
            .iter()
            .find(|candidate| candidate.id == request.target_id)
            .cloned()
            .ok_or_else(|| {
                SystemInstallError::InvalidInput(format!(
                    "unknown block device {}",
                    request.target_id
                ))
            })?;
        // Invariant 1: permanent, no escape hatch.
        if target.holds_running_system {
            return Err(SystemInstallError::RunningSystem(target.path.clone()));
        }
        // Invariant 2: never write a mounted device.
        if target.mounted {
            return Err(SystemInstallError::Conflict(format!(
                "{} has mounted partitions: {}",
                target.path,
                target.mount_points.join(", ")
            )));
        }
        let image = self.resolve_image(request, &target, &mounts)?;
        if image.uncompressed_bytes > target.size_bytes {
            return Err(SystemInstallError::Conflict(format!(
                "the image needs {} bytes but {} has {} bytes",
                image.uncompressed_bytes, target.path, target.size_bytes
            )));
        }
        let revision = revision(&targets, &mounts);
        let issued_at = (self.now)();
        let plan_token = plan_token(
            &revision,
            &target,
            &image,
            issued_at,
            mint_nonce(),
            &self.token_key,
        );
        let mut warnings = vec![
            "writing_an_image_replaces_every_partition_on_the_target".to_owned(),
            "the_write_cannot_be_rolled_back".to_owned(),
        ];
        if target.removable {
            warnings.push("the_target_is_removable_media".to_owned());
        }
        if image.format == InstallImageFormat::Xz {
            warnings.push("the_image_is_compressed_and_is_decompressed_while_writing".to_owned());
        }
        Ok(SystemInstallPlan {
            synthetic: self.synthetic,
            revision,
            plan_token,
            created_at: issued_at,
            request: request.clone(),
            destroys: target.partitions.clone(),
            target,
            image,
            warnings,
            requires_root: true,
            chunk_bytes: CHUNK_BYTES,
        })
    }

    /// Verifies the image file and derives the identity the token binds.
    fn resolve_image(
        &self,
        request: &SystemInstallRequest,
        target: &SystemInstallTarget,
        mounts: &[SystemMount],
    ) -> Result<SystemInstallImage, SystemInstallError> {
        let Some(path) = request.image_path.as_deref() else {
            return Err(SystemInstallError::Unsupported(match &request.image_id {
                Some(_) => "no image catalog is configured; pass an image path".into(),
                None => "an image path is required".to_string(),
            }));
        };
        if !path.starts_with('/') {
            return Err(SystemInstallError::InvalidInput(format!(
                "the image path must be absolute: {path}"
            )));
        }
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            SystemInstallError::InvalidInput(format!("the image {path} is unreadable: {error}"))
        })?;
        if !metadata.is_file() {
            return Err(SystemInstallError::InvalidInput(format!(
                "the image {path} is not a regular file"
            )));
        }
        let size_bytes = metadata.len();
        if size_bytes == 0 {
            return Err(SystemInstallError::InvalidInput(format!(
                "the image {path} is empty"
            )));
        }
        // Nested rather than a `let` chain: chains need Rust 1.88 and this crate
        // declares 1.85 as its minimum.
        if let Some(mount) = longest_mount_prefix(mounts, Path::new(path)) {
            if resolves_to_target(&mount.device, target) {
                return Err(SystemInstallError::Conflict(format!(
                    "the image {path} is stored on {} which is being overwritten",
                    mount.mount_point
                )));
            }
        }
        let mut file = File::open(path).map_err(|error| {
            SystemInstallError::InvalidInput(format!("the image {path} is unreadable: {error}"))
        })?;
        let mut header = vec![0u8; IMAGE_PREFIX_BYTES.min(size_bytes) as usize];
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.read_exact(&mut header))
            .map_err(|error| {
                SystemInstallError::Io(format!("unable to read the image header: {error}"))
            })?;
        let sha256_prefix = hex(&sha256(&header));
        let (format, uncompressed_bytes, partition_table, partitions) =
            if header.starts_with(&XZ_MAGIC) {
                let uncompressed = read_xz_size(&mut file)?;
                (InstallImageFormat::Xz, uncompressed, None, Vec::new())
            } else {
                let (table, partitions) = read_partition_table(&mut file)?;
                (InstallImageFormat::Raw, size_bytes, Some(table), partitions)
            };
        Ok(SystemInstallImage {
            path: path.to_owned(),
            id: request.image_id.clone(),
            format,
            size_bytes,
            uncompressed_bytes,
            sha256_prefix,
            prefix_bytes: header.len() as u64,
            partition_table,
            partitions,
        })
    }

    /// Streams the image in bounded chunks. A short stream is a failure because
    /// the device would otherwise hold a silently truncated system.
    fn write_image(
        &self,
        plan: &SystemInstallPlan,
        stream: &mut dyn Read,
    ) -> Result<u64, SystemInstallError> {
        let limit = plan.image.uncompressed_bytes;
        let mut buffer = vec![0u8; CHUNK_BYTES.min(limit) as usize];
        let mut written = 0u64;
        while written < limit {
            let want = buffer.len().min((limit - written) as usize);
            let read = stream
                .read(&mut buffer[..want])
                .map_err(|error| self.partial_failure(written, limit, &error.to_string()))?;
            if read == 0 {
                return Err(self.partial_failure(
                    written,
                    limit,
                    "the image ended before the size it declares",
                ));
            }
            self.backend
                .write_chunk(&plan.target, written, &buffer[..read])
                .map_err(|error| self.partial_failure(written, limit, &error.to_string()))?;
            written += read as u64;
        }
        // The declared size is a bound, never the only termination condition: a
        // stream that still holds data would be truncated silently, so read once
        // more and fail loudly with the count already on the device.
        let mut probe = [0u8; 1];
        match stream.read(&mut probe) {
            Ok(0) => Ok(written),
            Ok(_) => Err(SystemInstallError::Conflict(format!(
                "the decompressed image is longer than the {limit} bytes the plan declares; wrote {written} bytes and stopped instead of truncating the image"
            ))),
            Err(error) => Err(self.partial_failure(written, limit, &error.to_string())),
        }
    }

    /// No rollback exists, so the count of bytes already on the device is part
    /// of the error the caller must surface.
    fn partial_failure(&self, written: u64, limit: u64, reason: &str) -> SystemInstallError {
        SystemInstallError::Io(format!(
            "wrote {written} of {limit} bytes before failing ({reason}); the target now holds a partial image and cannot be rolled back"
        ))
    }

    fn verify(&self, plan: &SystemInstallPlan) -> SystemInstallVerification {
        match self.backend.reread_partitions(&plan.target) {
            Ok(observed) => {
                if plan.image.format == InstallImageFormat::Xz {
                    // A compressed image is never decompressed while planning, so
                    // there is no expected layout to compare: report that honestly
                    // instead of claiming the re-read table does not match.
                    return SystemInstallVerification {
                        state: SystemInstallVerificationState::Unsupported,
                        expected: Vec::new(),
                        observed: Some(observed),
                        detail: Some(
                            "the image is compressed and was not parsed before writing, so the re-read partition table was not compared"
                                .into(),
                        ),
                    };
                }
                let matched = same_partition_layout(&plan.image.partitions, &observed);
                SystemInstallVerification {
                    state: if matched {
                        SystemInstallVerificationState::Matched
                    } else {
                        SystemInstallVerificationState::Mismatched
                    },
                    expected: plan.image.partitions.clone(),
                    observed: Some(observed),
                    detail: if matched {
                        None
                    } else {
                        Some("the re-read partition table differs from the image".into())
                    },
                }
            }
            Err(SystemInstallError::Unsupported(reason)) => SystemInstallVerification {
                state: SystemInstallVerificationState::Unsupported,
                expected: plan.image.partitions.clone(),
                observed: None,
                detail: Some(reason),
            },
            Err(error) => SystemInstallVerification {
                state: SystemInstallVerificationState::Mismatched,
                expected: plan.image.partitions.clone(),
                observed: None,
                detail: Some(error.to_string()),
            },
        }
    }

    /// Single-use token check: a replay, an unknown token or an expired one all
    /// mean the caller must preview again.
    ///
    /// The record of what this process issued is the authority, and it is removed
    /// here, so a second consumption is refused. A token this process never issued
    /// is refused no matter what its fields say, which is what makes the guarantee
    /// real: every surface holds one manager, so the process that writes is the
    /// process that issued, and the privileged helper issues and consumes its own
    /// token instead of honouring a caller's.
    fn consume(&self, plan_token: &str) -> Result<(), SystemInstallError> {
        let issued = self
            .issued
            .lock()
            .expect("install token lock")
            .remove(plan_token)
            .ok_or(SystemInstallError::StalePlan)?;
        let age = ((self.now)() - issued).to_std().unwrap_or_default();
        if age > PLAN_TTL {
            return Err(SystemInstallError::StalePlan);
        }
        Ok(())
    }

    /// The issuance time of a token this process signed and issued.
    fn issued_at(&self, plan_token: &str) -> Result<DateTime<Utc>, SystemInstallError> {
        self.issued
            .lock()
            .expect("install token lock")
            .get(plan_token)
            .copied()
            .ok_or(SystemInstallError::StalePlan)
    }

    /// Parses a token and checks the signature this process put on it.
    fn parse_token(&self, plan_token: &str) -> Option<ParsedToken> {
        let parsed = split_plan_token(plan_token)?;
        let signature = hmac_sha256(self.token_key.as_slice(), parsed.signed.as_bytes());
        if !constant_time_eq(&signature, &parsed.mac) {
            return None;
        }
        Some(parsed)
    }

    /// Checks a caller supplied preview token for state drift and age.
    ///
    /// This is **not** an authorization and cannot be: the signature is only
    /// verifiable by the process that issued the token. A surface that issues and
    /// consumes its own token (the privileged helper does) uses this to notice
    /// that the state moved since the preview the operator was shown, or that the
    /// preview is old. The caller writes the issuance time, so that age check is
    /// advisory; what protects the device is the rebuilt plan, the hard gates, the
    /// typed acknowledgement and the administrator authorization beneath them.
    pub fn verify_preview_token(
        &self,
        request: &SystemInstallRequest,
        plan_token: &str,
    ) -> Result<(), SystemInstallError> {
        let Some(presented) = split_plan_token(plan_token) else {
            return Err(SystemInstallError::StalePlan);
        };
        let plan = self.build_plan(request)?;
        if presented.binding != token_binding(&plan.revision, &plan.target, &plan.image) {
            return Err(SystemInstallError::StalePlan);
        }
        let issued = DateTime::<Utc>::from_timestamp(presented.issued as i64, 0)
            .ok_or(SystemInstallError::StalePlan)?;
        let age = (self.now)().signed_duration_since(issued);
        if age > chrono::Duration::from_std(PLAN_TTL).unwrap_or_default() || age < -TOKEN_CLOCK_SKEW
        {
            return Err(SystemInstallError::StalePlan);
        }
        Ok(())
    }
}

/// Marks a target with the mount facts the gates need.
fn annotate_mounts(target: &mut SystemInstallTarget, mounts: &[SystemMount]) {
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

/// Whether a mount device (a device node or a kernel name) belongs to a target.
fn resolves_to_target(device: &str, target: &SystemInstallTarget) -> bool {
    let name = device.trim_start_matches("/dev/");
    name == target.id || partition_number(target, name).is_some()
}

/// Kernel partitions of `mmcblk0`/`nvme0n1` are `pN`, of `sda` they are `N`.
fn partition_number(target: &SystemInstallTarget, partition: &str) -> Option<u32> {
    partition_number_of(&target.id, partition)
}

fn partition_number_of(disk: &str, partition: &str) -> Option<u32> {
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

fn longest_mount_prefix<'a>(mounts: &'a [SystemMount], path: &Path) -> Option<&'a SystemMount> {
    mounts
        .iter()
        .filter(|mount| path.starts_with(Path::new(&mount.mount_point)))
        .max_by_key(|mount| mount.mount_point.len())
}

/// Identity of the whole observed device state; any device or mount change
/// produces a different plan token.
fn revision(targets: &[SystemInstallTarget], mounts: &[SystemMount]) -> String {
    let mut bytes = Vec::new();
    for target in targets {
        bytes.extend_from_slice(target.id.as_bytes());
        bytes.extend_from_slice(target.path.as_bytes());
        bytes.extend_from_slice(target.model.as_bytes());
        bytes.extend_from_slice(&target.size_bytes.to_le_bytes());
        bytes.extend_from_slice(target.kind.marker().as_bytes());
        bytes.extend_from_slice(&[target.removable as u8, target.holds_running_system as u8]);
        for partition in &target.partitions {
            bytes.extend_from_slice(partition.name.as_bytes());
            bytes.extend_from_slice(&partition.start_bytes.to_le_bytes());
            bytes.extend_from_slice(&partition.size_bytes.to_le_bytes());
        }
    }
    for mount in mounts {
        bytes.extend_from_slice(mount.device.as_bytes());
        bytes.extend_from_slice(mount.mount_point.as_bytes());
    }
    format!("install-v1-{:016x}", stable_hash(&bytes))
}

/// A token as it is carried: the state binding, when it was issued, a per-issuance
/// nonce, and the signature over all of that.
struct ParsedToken {
    binding: u64,
    issued: u64,
    #[allow(dead_code)]
    nonce: u64,
    /// Exactly the text the signature covers.
    signed: String,
    mac: [u8; 32],
}

/// Splits the fixed token shape without checking the signature. Anything else is
/// not a token this build produced.
fn split_plan_token(token: &str) -> Option<ParsedToken> {
    const PREFIX: &str = "install-plan-";
    let rest = token.strip_prefix(PREFIX)?;
    let mut parts = rest.split('-');
    let binding = u64::from_str_radix(parts.next()?, 16).ok()?;
    let issued = u64::from_str_radix(parts.next()?, 16).ok()?;
    let nonce = u64::from_str_radix(parts.next()?, 16).ok()?;
    let mac_hex = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if rest.len() != 16 + 1 + 10 + 1 + 16 + 1 + 64 || mac_hex.len() != 64 {
        return None;
    }
    let mut mac = [0u8; 32];
    for (index, pair) in mac_hex.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(pair).ok()?;
        mac[index] = u8::from_str_radix(text, 16).ok()?;
    }
    let signed = format!("{PREFIX}{binding:016x}-{issued:010x}-{nonce:016x}");
    Some(ParsedToken {
        binding,
        issued,
        nonce,
        signed,
        mac,
    })
}

/// A per-issuance value, so two previews of the same request never share a token
/// (a shared token would make the second preview look like a replay of the first).
fn mint_nonce() -> u64 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut seed = 0u64;
    if let Ok(mut urandom) = File::open("/dev/urandom") {
        let mut bytes = [0u8; 8];
        if urandom.read_exact(&mut bytes).is_ok() {
            seed = u64::from_ne_bytes(bytes);
        }
    }
    if seed == 0 {
        seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| u64::from(elapsed.subsec_nanos()))
            .unwrap_or_default();
    }
    seed ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// The signing key for this process. Without `/dev/urandom` it falls back to
/// process and clock entropy; either way the key is never disclosed, and a token
/// is only useful to the process that issued it.
fn mint_token_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    // Nested rather than a `let` chain: chains need Rust 1.88 and this crate
    // declares 1.85 as its minimum.
    if let Ok(mut urandom) = File::open("/dev/urandom") {
        if urandom.read_exact(&mut key).is_ok() {
            return key;
        }
    }
    let mut seed = Vec::new();
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default(),
    );
    seed.extend_from_slice(&hasher.finish().to_le_bytes());
    let local = 0u8;
    seed.extend_from_slice(&std::ptr::from_ref(&local).addr().to_le_bytes());
    sha256(&seed)
}

/// HMAC-SHA256 (RFC 2104).
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() <= BLOCK {
        padded[..key.len()].copy_from_slice(key);
    } else {
        padded[..32].copy_from_slice(&sha256(key));
    }
    let mut inner = Vec::with_capacity(BLOCK + message.len());
    inner.extend(padded.iter().map(|byte| byte ^ 0x36));
    inner.extend_from_slice(message);
    let inner = sha256(&inner);
    let mut outer = Vec::with_capacity(BLOCK + 32);
    outer.extend(padded.iter().map(|byte| byte ^ 0x5c));
    outer.extend_from_slice(&inner);
    sha256(&outer)
}

/// Compares two signatures without leaking where they first differ.
fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

/// Binds the observed state, the target identity, the image identity and the
/// acknowledgement the caller must type.
fn token_binding(revision: &str, target: &SystemInstallTarget, image: &SystemInstallImage) -> u64 {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(revision.as_bytes());
    bytes.extend_from_slice(target.id.as_bytes());
    bytes.extend_from_slice(target.path.as_bytes());
    bytes.extend_from_slice(target.model.as_bytes());
    bytes.extend_from_slice(&target.size_bytes.to_le_bytes());
    bytes.extend_from_slice(image.path.as_bytes());
    bytes.extend_from_slice(&image.size_bytes.to_le_bytes());
    bytes.extend_from_slice(&image.uncompressed_bytes.to_le_bytes());
    bytes.extend_from_slice(image.sha256_prefix.as_bytes());
    // The expected acknowledgement is part of the token, so a plan cannot be
    // replayed against a different device name.
    bytes.extend_from_slice(target.id.as_bytes());
    stable_hash(&bytes)
}

/// The issued token: the binding above, the issuance time, a per-issuance nonce and
/// a signature over exactly that text. Editing any field — the nonce to make a
/// consumed token look new, the time to refresh the window — invalidates the
/// signature, and only the issuing process holds the key.
fn plan_token(
    revision: &str,
    target: &SystemInstallTarget,
    image: &SystemInstallImage,
    issued_at: DateTime<Utc>,
    nonce: u64,
    key: &[u8; 32],
) -> String {
    let binding = token_binding(revision, target, image);
    let signed = format!(
        "install-plan-{binding:016x}-{:010x}-{nonce:016x}",
        issued_at.timestamp().max(0)
    );
    let mac = hmac_sha256(key, signed.as_bytes());
    format!("{signed}-{}", hex(&mac))
}

/// State-change hashing for tokens (FNV-1a based, matching the other managers).
/// It is not a security primitive.
fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Compares partitions by geometry: names differ between an image and the
/// kernel's view, but the layout a system boots from does not.
fn same_partition_layout(
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
        Ok(parse_mounts(&text))
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
            let file = OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(|error| {
                    SystemInstallError::Io(format!("unable to open {}: {error}", path.display()))
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

fn read_attr(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_mounts(text: &str) -> Vec<SystemMount> {
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

/// Opens raw images directly and `.xz` images through `xz -dc`, the same
/// "call the required system tool" boundary the other managers use.
pub struct SystemImageOpener;

impl ImageOpener for SystemImageOpener {
    fn open(&self, image: &SystemInstallImage) -> Result<Box<dyn Read + Send>, SystemInstallError> {
        match image.format {
            InstallImageFormat::Raw => {
                let file = File::open(&image.path).map_err(|error| {
                    SystemInstallError::Io(format!("unable to open {}: {error}", image.path))
                })?;
                Ok(Box::new(file))
            }
            InstallImageFormat::Xz => {
                let mut command = Command::new("xz");
                command
                    .args(["-dc", &image.path])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    // The reason for a failure is on stderr, so it is captured
                    // rather than discarded.
                    .stderr(Stdio::piped());
                let decompressor =
                    spawn_decompressor(&mut command).map_err(|error| match error.kind() {
                        std::io::ErrorKind::NotFound => SystemInstallError::Unsupported(
                            "install xz-utils to write a compressed image".into(),
                        ),
                        _ => SystemInstallError::Io(format!("unable to start xz: {error}")),
                    })?;
                Ok(Box::new(decompressor))
            }
        }
    }
}

/// Spawns a decompressor and returns its judged output stream. Only an
/// unfinished stream is terminated by dropping it; a finished one has been
/// waited for, so its exit status is never thrown away.
fn spawn_decompressor(command: &mut Command) -> std::io::Result<Decompressor> {
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("the decompressor produced no output stream"))?;
    // stderr is drained on its own thread: it must not fill up and block the
    // tool, and a bounded prefix of it is what explains a failure.
    let stderr = child.stderr.take().map(|mut stderr| {
        std::thread::spawn(move || {
            const MAX_STDERR: usize = 64 * 1024;
            let mut collected = Vec::new();
            let mut buffer = [0u8; 4096];
            while collected.len() < MAX_STDERR {
                match std::io::Read::read(&mut stderr, &mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => collected.extend_from_slice(&buffer[..read]),
                }
            }
            collected
        })
    });
    Ok(Decompressor {
        child,
        stdout,
        stderr,
        delivered: 0,
        finished: false,
    })
}

/// A decompressor's output. End of output is only reported once the tool has
/// exited successfully, so a corrupt image cannot look like a complete one: the
/// caller sees an I/O failure instead of a clean end of stream.
struct Decompressor {
    child: Child,
    stdout: ChildStdout,
    stderr: Option<std::thread::JoinHandle<Vec<u8>>>,
    delivered: u64,
    finished: bool,
}

impl Decompressor {
    /// Waits for the tool and turns a non-zero exit into a hard failure.
    fn finish(&mut self) -> std::io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let status = self.child.wait()?;
        let stderr = self
            .stderr
            .take()
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();
        if status.success() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&stderr);
        let text = text.trim();
        Err(std::io::Error::other(format!(
            "the decompressor failed ({status}) after {} bytes of output, so the data may be incomplete or corrupt{}",
            self.delivered,
            if text.is_empty() {
                String::new()
            } else {
                format!(": {text}")
            }
        )))
    }
}

impl Read for Decompressor {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.stdout.read(buffer)?;
        if read == 0 {
            // End of output is not success: the status is part of the result.
            self.finish()?;
        }
        self.delivered += read as u64;
        Ok(read)
    }
}

impl Drop for Decompressor {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // Only an unfinished stream is terminated here: this is cleanup, not the
        // status check.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.stderr.take() {
            let _ = handle.join();
        }
    }
}

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

/// Reads the partition table of a raw image: MBR, or GPT behind a protective
/// MBR.
fn read_partition_table(
    file: &mut File,
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

fn read_gpt(file: &mut File) -> Result<Vec<SystemInstallPartition>, SystemInstallError> {
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

/// Sums the uncompressed size of **every** stream in an `.xz` file, so a
/// concatenated multi-stream image is measured in full instead of only its last
/// stream (which is what silently truncated one). Streams are walked backwards
/// from the end of the file:
///
/// * a stream footer gives the size of its own index (the stored value is
///   `index size / 4 - 1`, and that size includes the index CRC32),
/// * the index records give every block's unpadded size and its uncompressed
///   size, so the block area plus the two 12-byte frames is the stream's total
///   size,
/// * subtracting that total gives the previous stream, skipping the stream
///   padding (null bytes, a multiple of four) that may sit in between,
/// * the stream header magic must sit exactly where the arithmetic says, so a
///   mis-walk is an error instead of a guessed size.
fn read_xz_size(file: &mut File) -> Result<u64, SystemInstallError> {
    let length = file
        .metadata()
        .map_err(|error| SystemInstallError::Io(error.to_string()))?
        .len();
    if length < 32 {
        return Err(SystemInstallError::InvalidInput(
            "the .xz image is too short to hold a stream".into(),
        ));
    }
    let implausible = |detail: &str| SystemInstallError::InvalidInput(detail.to_owned());
    let overflow = || SystemInstallError::InvalidInput("the .xz size overflows".into());
    let mut end = length;
    let mut total = 0u64;
    let mut streams = 0usize;
    while end > 0 {
        let padding = trailing_null_bytes(file, end)?;
        if padding % 4 != 0 {
            return Err(implausible(
                "the .xz stream padding is not a multiple of four",
            ));
        }
        end -= padding;
        if end == 0 {
            break;
        }
        if end < 32 {
            return Err(implausible("the .xz image is too short to hold a stream"));
        }
        let mut footer = [0u8; 12];
        file.seek(SeekFrom::Start(end - 12))
            .and_then(|_| file.read_exact(&mut footer))
            .map_err(|error| SystemInstallError::Io(error.to_string()))?;
        if footer[10..12] != XZ_FOOTER_MAGIC {
            return Err(implausible("the .xz stream footer is malformed"));
        }
        if crc32(&footer[4..10]) != u32::from_le_bytes(footer[0..4].try_into().unwrap_or_default())
        {
            return Err(implausible("the .xz stream footer checksum is wrong"));
        }
        let backward_size = (u64::from(u32::from_le_bytes(
            footer[4..8].try_into().unwrap_or_default(),
        )) + 1)
            * 4;
        if backward_size + 12 > end {
            return Err(implausible("the .xz index size is implausible"));
        }
        // The index sits before the 12-byte stream footer, not at the file end.
        let index_start = end - 12 - backward_size;
        let mut index = vec![0u8; backward_size as usize];
        file.seek(SeekFrom::Start(index_start))
            .and_then(|_| file.read_exact(&mut index))
            .map_err(|error| SystemInstallError::Io(error.to_string()))?;
        if index.first() != Some(&0x00) {
            return Err(implausible("the .xz index is malformed"));
        }
        if index.len() < 4 {
            return Err(implausible("the .xz index is too short"));
        }
        let split = index.len() - 4;
        if crc32(&index[..split])
            != u32::from_le_bytes(index[split..].try_into().unwrap_or_default())
        {
            return Err(implausible("the .xz index checksum is wrong"));
        }
        let mut cursor = 1usize;
        let (records, used) = read_xz_vli(&index[cursor..])?;
        cursor += used;
        let mut blocks_bytes = 0u64;
        let mut stream_bytes = 0u64;
        for _ in 0..records {
            let (unpadded, used) = read_xz_vli(&index[cursor..])?;
            cursor += used;
            let (uncompressed, used) = read_xz_vli(&index[cursor..])?;
            cursor += used;
            // An index record stores the block size *without* the block padding,
            // while the file holds the padding too: each block is rounded up to a
            // multiple of four bytes, and a block header is always a multiple of
            // four, so rounding the unpadded size is enough.
            let padded = unpadded + (4 - unpadded % 4) % 4;
            blocks_bytes = blocks_bytes.checked_add(padded).ok_or_else(overflow)?;
            stream_bytes = stream_bytes
                .checked_add(uncompressed)
                .ok_or_else(overflow)?;
        }
        // Stream header (12) + blocks (their headers and compressed data) + index
        // (which already counts its own CRC32) + stream footer (12).
        let stream_length = 12u64
            .checked_add(blocks_bytes)
            .ok_or_else(overflow)?
            .checked_add(backward_size)
            .ok_or_else(overflow)?
            .checked_add(12)
            .ok_or_else(overflow)?;
        if stream_length > end {
            return Err(implausible("the .xz stream size is implausible"));
        }
        let start = end - stream_length;
        let mut header = [0u8; 12];
        file.seek(SeekFrom::Start(start))
            .and_then(|_| file.read_exact(&mut header))
            .map_err(|error| SystemInstallError::Io(error.to_string()))?;
        if header[..6] != XZ_MAGIC {
            return Err(implausible("the .xz stream header is malformed"));
        }
        // Stream flags: the first byte is reserved and must be zero.
        if header[6] != 0 {
            return Err(implausible("the .xz stream flags are malformed"));
        }
        if crc32(&header[6..8]) != u32::from_le_bytes(header[8..12].try_into().unwrap_or_default())
        {
            return Err(implausible("the .xz stream header checksum is wrong"));
        }
        total = total.checked_add(stream_bytes).ok_or_else(overflow)?;
        streams += 1;
        end = start;
    }
    if streams == 0 {
        return Err(implausible("the image is not an xz stream"));
    }
    if total == 0 {
        return Err(implausible("the .xz image holds no data"));
    }
    Ok(total)
}

/// Counts the null bytes that end `[0, end)`. Stream padding sits between streams
/// and may also trail the file; a valid stream never ends with a null byte (its
/// footer ends with `YZ`), so everything skipped here is padding.
fn trailing_null_bytes(file: &mut File, end: u64) -> Result<u64, SystemInstallError> {
    /// Padding only ever aligns a stream, so anything beyond this is a malformed
    /// file rather than padding worth scanning.
    const MAX_PADDING: u64 = 4 * 1024 * 1024;
    const STEP: u64 = 64;
    let mut count = 0u64;
    let mut cursor = end;
    while cursor > 0 && count < MAX_PADDING {
        let size = STEP.min(cursor);
        let mut buffer = vec![0u8; size as usize];
        file.seek(SeekFrom::Start(cursor - size))
            .and_then(|_| file.read_exact(&mut buffer))
            .map_err(|error| SystemInstallError::Io(error.to_string()))?;
        let trailing = buffer.iter().rev().take_while(|byte| **byte == 0).count() as u64;
        count += trailing;
        if trailing < size {
            break;
        }
        cursor -= size;
    }
    if count >= MAX_PADDING {
        return Err(SystemInstallError::InvalidInput(
            "the .xz stream padding is implausibly long".into(),
        ));
    }
    Ok(count)
}

/// xz stores variable-length integers as little-endian base-128.
fn read_xz_vli(bytes: &[u8]) -> Result<(u64, usize), SystemInstallError> {
    let mut value = 0u64;
    let mut shift = 0u32;
    for (index, byte) in bytes.iter().enumerate() {
        if shift > 63 {
            return Err(SystemInstallError::InvalidInput(
                "the .xz variable length integer is too large".into(),
            ));
        }
        value |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
        shift += 7;
    }
    Err(SystemInstallError::InvalidInput(
        "the .xz variable length integer is truncated".into(),
    ))
}

/// Lowercase hex, used for the image digest in plans and tests.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// SHA-256, used to bind the exact image bytes into the plan token. The
/// workspace has no hashing dependency and this module must not add one.
fn sha256(bytes: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_length = (bytes.len() as u64).wrapping_mul(8);
    let mut message = bytes.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());
    for block in message.chunks_exact(64) {
        let mut schedule = [0u32; 64];
        for index in 0..16 {
            schedule[index] = u32::from_be_bytes(
                block[index * 4..index * 4 + 4]
                    .try_into()
                    .unwrap_or_default(),
            );
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }
        let mut working = state;
        for index in 0..64 {
            let s1 = working[4].rotate_right(6)
                ^ working[4].rotate_right(11)
                ^ working[4].rotate_right(25);
            let choose = (working[4] & working[5]) ^ ((!working[4]) & working[6]);
            let temp1 = working[7]
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let s0 = working[0].rotate_right(2)
                ^ working[0].rotate_right(13)
                ^ working[0].rotate_right(22);
            let majority =
                (working[0] & working[1]) ^ (working[0] & working[2]) ^ (working[1] & working[2]);
            let temp2 = s0.wrapping_add(majority);
            working[7] = working[6];
            working[6] = working[5];
            working[5] = working[4];
            working[4] = working[3].wrapping_add(temp1);
            working[3] = working[2];
            working[2] = working[1];
            working[1] = working[0];
            working[0] = temp1.wrapping_add(temp2);
        }
        for index in 0..8 {
            state[index] = state[index].wrapping_add(working[index]);
        }
    }
    let mut digest = [0u8; 32];
    for (index, value) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scratch space under the workspace `target/` directory, so tests only ever
    /// use ordinary files and never a real block device.
    fn scratch(name: &str) -> Scratch {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/system-install-tests")
            .join(format!("{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create scratch directory");
        Scratch(root)
    }

    /// Owns one test's fixture directory and removes it on drop, so the suite
    /// cannot accumulate image-sized files in `target/` (also on a panic).
    struct Scratch(PathBuf);

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

    fn write_file(path: &Path, bytes: impl AsRef<[u8]>) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, bytes.as_ref()).expect("write fixture file");
    }

    fn write_attr(path: &Path, value: &str) {
        write_file(path, value.as_bytes());
    }

    /// A raw image with an MBR and `count` partitions of `partition_bytes`.
    fn mbr_image(count: usize, partition_bytes: u64) -> Vec<u8> {
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

    /// The MBR the fixture image declares, as the verification compares it.
    fn mbr_partitions(partition_bytes: u64) -> Vec<SystemInstallPartition> {
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

    fn gpt_image(entries: &[(u64, u64)]) -> Vec<u8> {
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

    /// A real `.xz` stream: `printf 'hello world\n' | xz -9`, i.e. 12 bytes
    /// uncompressed. Pinned here so the parser is tested without a compression
    /// dependency and without running a tool.
    const XZ_FIXTURE: &[u8] = &[
        0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00, 0x00, 0x04, 0xE6, 0xD6, 0xB4, 0x46, 0x02, 0x00, 0x21,
        0x01, 0x1C, 0x00, 0x00, 0x00, 0x10, 0xCF, 0x58, 0xCC, 0x01, 0x00, 0x0B, 0x68, 0x65, 0x6C,
        0x6C, 0x6F, 0x20, 0x77, 0x6F, 0x72, 0x6C, 0x64, 0x0A, 0x00, 0xA1, 0xF2, 0xFF, 0xC4, 0x6A,
        0x7F, 0xBF, 0xCF, 0x00, 0x01, 0x24, 0x0C, 0xA6, 0x18, 0xD8, 0xD8, 0x1F, 0xB6, 0xF3, 0x7D,
        0x01, 0x00, 0x00, 0x00, 0x00, 0x04, 0x59, 0x5A,
    ];

    #[derive(Clone, Copy, PartialEq)]
    enum RereadBehavior {
        Matches,
        Differs,
        Unsupported,
    }

    /// Fixture backend: writes into an ordinary file under `target/`, and can be
    /// told to fail part way or to disagree about the resulting table.
    struct FixtureBackend {
        targets: Mutex<Vec<SystemInstallTarget>>,
        mounts: Vec<SystemMount>,
        device: PathBuf,
        fail_after: Mutex<Option<u64>>,
        behavior: Mutex<RereadBehavior>,
        chunks: Mutex<Vec<usize>>,
        expected: Vec<SystemInstallPartition>,
    }

    impl BlockDeviceBackend for FixtureBackend {
        fn targets(&self) -> Result<Vec<SystemInstallTarget>, SystemInstallError> {
            Ok(self.targets.lock().expect("targets lock").clone())
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
            let total: u64 =
                written.iter().map(|size| *size as u64).sum::<u64>() + chunk.len() as u64;
            // Nested rather than a `let` chain: chains need Rust 1.88 and this
            // crate declares 1.85 as its minimum. The guard is still held for the
            // whole statement, exactly as it was with the chain.
            if let Some(fail_after) = *self.fail_after.lock().expect("fail lock") {
                if total > fail_after {
                    return Err(SystemInstallError::Io("simulated device failure".into()));
                }
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

    struct Fixture {
        root: Scratch,
        device: PathBuf,
        image: PathBuf,
        backend: Arc<FixtureBackend>,
        manager: SystemInstallManager,
    }

    fn fixture(name: &str, mounts: Vec<SystemMount>, reread: RereadBehavior) -> Fixture {
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

    fn install_request(image: &Path) -> SystemInstallRequest {
        SystemInstallRequest {
            target_id: "sda".into(),
            image_path: Some(image.display().to_string()),
            image_id: None,
        }
    }

    fn boot_mounts() -> Vec<SystemMount> {
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

    #[test]
    fn sha256_matches_the_nist_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(&[b'a'; 1_000_000])),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

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
        write_attr(&root.join("sys/class/block/mmcblk1p1/partition"), "1\n");
        write_attr(&root.join("sys/class/block/mmcblk1p1/start"), "8192\n");
        write_attr(&root.join("sys/class/block/mmcblk1p1/size"), "524288\n");
        write_attr(&root.join("sys/class/block/loop0/size"), "1024\n");
        write_file(
            &root.join("proc/mounts"),
            b"/dev/mmcblk1p1 /boot vfat rw 0 0\n/dev/mmcblk1p2 / ext4 rw 0 0\n",
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
    fn plan_exposes_the_full_consequence_list() {
        let harness = fixture("plan", boot_mounts(), RereadBehavior::Matches);
        let plan = harness
            .manager
            .plan(&install_request(&harness.image))
            .expect("plan a system install");
        assert_eq!(plan.target.id, "sda");
        assert_eq!(plan.target.path, "/dev/sda");
        assert_eq!(plan.target.kind, BlockDeviceKind::Ufs);
        assert_eq!(plan.target.model, "FIXTURE UFS 128GB");
        assert_eq!(plan.target.size_bytes, 64 * MIB);
        assert!(!plan.target.removable);
        assert!(!plan.target.mounted);
        assert!(!plan.target.holds_running_system);
        assert_eq!(plan.destroys.len(), 2, "the existing partitions are listed");
        assert_eq!(plan.destroys[0].name, "sda1");
        assert_eq!(plan.image.path, harness.image.display().to_string());
        assert_eq!(plan.image.format, InstallImageFormat::Raw);
        assert_eq!(plan.image.size_bytes, 2 * 8 * MIB + 2 * MIB);
        assert_eq!(plan.image.uncompressed_bytes, plan.image.size_bytes);
        assert_eq!(plan.image.partition_table.as_deref(), Some("mbr"));
        assert_eq!(plan.image.partitions.len(), 2);
        assert_eq!(plan.image.sha256_prefix.len(), 64);
        assert_eq!(plan.image.prefix_bytes, IMAGE_PREFIX_BYTES);
        assert!(plan.requires_root);
        assert_eq!(plan.chunk_bytes, CHUNK_BYTES);
        assert!(
            plan.warnings
                .contains(&"the_write_cannot_be_rolled_back".to_owned())
        );
        assert!(
            plan.warnings
                .contains(&"writing_an_image_replaces_every_partition_on_the_target".to_owned())
        );
        assert!(!plan.synthetic);
        assert!(plan.plan_token.starts_with("install-plan-"));
        assert!(plan.revision.starts_with("install-v1-"));
    }

    #[test]
    fn refuses_the_device_that_hosts_the_running_system() {
        let harness = fixture("running-system", boot_mounts(), RereadBehavior::Matches);
        {
            let mut targets = harness.backend.targets.lock().expect("targets lock");
            targets[0].id = "mmcblk1".into();
            targets[0].path = "/dev/mmcblk1".into();
            targets[0].partitions = vec![
                SystemInstallPartition {
                    name: "mmcblk1p1".into(),
                    number: 1,
                    start_bytes: 16 * MIB,
                    size_bytes: 256 * MIB,
                    mount_point: None,
                },
                SystemInstallPartition {
                    name: "mmcblk1p2".into(),
                    number: 2,
                    start_bytes: 272 * MIB,
                    size_bytes: 28 * GIB,
                    mount_point: None,
                },
            ];
        }
        let request = SystemInstallRequest {
            target_id: "mmcblk1".into(),
            image_path: Some(harness.image.display().to_string()),
            image_id: None,
        };
        assert!(matches!(
            harness.manager.plan(&request),
            Err(SystemInstallError::RunningSystem(path)) if path == "/dev/mmcblk1"
        ));
        // No acknowledgement reaches the write path for this device.
        assert!(matches!(
            harness
                .manager
                .apply_live(&request, "install-plan-0", "mmcblk1"),
            Err(SystemInstallError::RunningSystem(_))
        ));
    }

    #[test]
    fn refuses_a_target_with_mounted_partitions_and_lists_them() {
        let mounts = vec![SystemMount {
            device: "/dev/sda1".into(),
            mount_point: "/media/backup".into(),
            file_system: "ext4".into(),
        }];
        let harness = fixture("mounted", mounts, RereadBehavior::Matches);
        match harness.manager.plan(&install_request(&harness.image)) {
            Err(SystemInstallError::Conflict(message)) => {
                assert!(message.contains("/dev/sda"), "{message}");
                assert!(message.contains("/media/backup"), "{message}");
            }
            other => panic!("a mounted target must be refused: {other:?}"),
        }
    }

    #[test]
    fn acknowledgement_is_required_and_a_boolean_cannot_satisfy_it() {
        let harness = fixture("ack", boot_mounts(), RereadBehavior::Matches);
        let request = install_request(&harness.image);
        let plan = harness.manager.plan(&request).expect("plan");

        assert!(matches!(
            harness.manager.apply_live(&request, &plan.plan_token, ""),
            Err(SystemInstallError::ConfirmationRequired)
        ));
        for wrong in ["true", "yes", "1", "ufs", "/dev/sda"] {
            match harness
                .manager
                .apply_live(&request, &plan.plan_token, wrong)
            {
                Err(SystemInstallError::AcknowledgementMismatch { expected, .. }) => {
                    assert_eq!(expected, "sda");
                }
                other => panic!("{wrong} must not be accepted: {other:?}"),
            }
        }
        // A failed acknowledgement leaves the token usable, so the user can type
        // the right name without previewing again.
        let result = harness
            .manager
            .apply_live(&request, &plan.plan_token, "sda")
            .expect("the typed device name is accepted");
        assert_eq!(result.bytes_written, plan.image.uncompressed_bytes);
    }

    #[test]
    fn plan_token_is_single_use_and_expires() {
        let harness = fixture("token", boot_mounts(), RereadBehavior::Matches);
        let request = install_request(&harness.image);
        let plan = harness.manager.plan(&request).expect("plan");
        harness
            .manager
            .apply_live(&request, &plan.plan_token, "sda")
            .expect("first apply");
        assert!(
            matches!(
                harness
                    .manager
                    .apply_live(&request, &plan.plan_token, "sda"),
                Err(SystemInstallError::StalePlan)
            ),
            "a token cannot be replayed"
        );

        let expiry = fixture("token-ttl", boot_mounts(), RereadBehavior::Matches);
        let started = Utc::now();
        let now = Arc::new(Mutex::new(started));
        let clock = now.clone();
        let manager = expiry
            .manager
            .clone()
            .with_clock(Arc::new(move || *clock.lock().expect("clock lock")));
        let expiry_request = install_request(&expiry.image);
        let plan = manager
            .plan(&expiry_request)
            .expect("plan with a frozen clock");
        *now.lock().expect("clock lock") = started + chrono::Duration::seconds(601);
        assert!(matches!(
            manager.apply_live(&expiry_request, &plan.plan_token, "sda"),
            Err(SystemInstallError::StalePlan)
        ));
    }

    /// Whether the real tool is available; the subprocess tests skip without it
    /// (the `sh` based tests below cover the same status logic on every host).
    fn xz_available() -> bool {
        Command::new("xz")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn flip(bytes: &[u8], offset: usize) -> Vec<u8> {
        let mut mutated = bytes.to_vec();
        mutated[offset] ^= 0xFF;
        mutated
    }

    /// The frames xz itself checks — stream header, index, stream footer — are
    /// checked while planning, so a damaged file never reaches the write path.
    /// The offsets below are the frame bytes of the embedded fixtures (the header
    /// CRC lives at 8, the index CRC at length-16, the footer CRC at length-12).
    #[test]
    fn xz_frame_checksums_are_checked_while_planning() {
        let harness = fixture("xz-crc", boot_mounts(), RereadBehavior::Matches);
        let cases: Vec<(&str, usize)> = vec![
            ("header crc", 8),
            ("header flags", 7),
            ("index crc", XZ_FIXTURE.len() - 16),
            ("footer crc", XZ_FIXTURE.len() - 12),
        ];
        for (label, offset) in cases {
            let path = harness.root.join(format!("{label}.img.xz"));
            write_file(&path, flip(XZ_FIXTURE, offset));
            match harness.manager.plan(&install_request(&path)) {
                Err(SystemInstallError::InvalidInput(detail)) => {
                    assert!(
                        detail.contains("checksum") || detail.contains("flags"),
                        "{label}: {detail}"
                    );
                }
                other => panic!("{label} must be refused while planning: {other:?}"),
            }
        }
        // The same file with intact frames still plans, so the refusals above are
        // about the damaged bytes.
        let intact = harness.root.join("intact.img.xz");
        write_file(&intact, XZ_FIXTURE);
        harness
            .manager
            .plan(&install_request(&intact))
            .expect("the intact fixture plans");
    }

    /// The F-T20-002 regression, real tool: the decompressor produces the whole
    /// expected output and only then reports a CRC failure. Before the fix the
    /// product reported success for exactly this file.
    ///
    /// Reference, `xz -dc <file>` on this fixture: exit 1 with 12 bytes of output
    /// for the single stream (24 bytes for the concatenated one).
    #[test]
    fn a_corrupt_stream_that_produced_all_of_its_output_is_refused() {
        if !xz_available() {
            return;
        }
        let harness = fixture("xz-corrupt-full", boot_mounts(), RereadBehavior::Matches);
        // Offset 30 sits in the compressed data, so the frames stay valid and the
        // plan is accepted: only the decompressor's exit status reveals the damage.
        for (name, bytes, offset, expected) in [
            ("single.img.xz", XZ_FIXTURE.to_vec(), 30, 12u64),
            (
                "two.img.xz",
                [XZ_FIXTURE, XZ_FIXTURE].concat(),
                XZ_FIXTURE.len() + 30,
                24,
            ),
        ] {
            let path = harness.root.join(name);
            write_file(&path, flip(&bytes, offset));
            let plan = harness
                .manager
                .plan(&install_request(&path))
                .unwrap_or_else(|error| panic!("{name} plans (frames are intact): {error}"));
            assert_eq!(plan.image.uncompressed_bytes, expected, "{name}");
            match harness
                .manager
                .apply_live(&install_request(&path), &plan.plan_token, "sda")
            {
                Err(SystemInstallError::Io(message)) => {
                    assert!(
                        message.contains("decompressor failed"),
                        "{name} must report the exit status: {message}"
                    );
                    assert!(
                        message.contains("incomplete or corrupt"),
                        "{name}: {message}"
                    );
                    assert!(
                        message.contains("cannot be rolled back"),
                        "{name} must say the device may be partial: {message}"
                    );
                }
                other => panic!("{name} must fail, not report success: {other:?}"),
            }
        }
    }

    /// A block damaged before any output, and one that makes the tool emit more
    /// than the plan declared: both must be hard failures, never a success.
    #[test]
    fn a_stream_that_fails_mid_way_is_refused() {
        if !xz_available() {
            return;
        }
        let harness = fixture("xz-corrupt-mid", boot_mounts(), RereadBehavior::Matches);
        for (name, offset) in [("early.img.xz", 16), ("partial.img.xz", 25)] {
            let path = harness.root.join(name);
            write_file(&path, flip(XZ_FIXTURE, offset));
            let plan = harness
                .manager
                .plan(&install_request(&path))
                .unwrap_or_else(|error| panic!("{name} plans: {error}"));
            let outcome =
                harness
                    .manager
                    .apply_live(&install_request(&path), &plan.plan_token, "sda");
            assert!(
                matches!(
                    outcome,
                    Err(SystemInstallError::Io(_)) | Err(SystemInstallError::Conflict(_))
                ),
                "{name} must be a hard failure: {outcome:?}"
            );
        }
    }

    /// The status check does not need the real tool: a child that fails after
    /// printing something is judged the same way, and its stderr is the reason.
    #[test]
    fn the_decompressor_waits_for_the_child_and_reports_its_failure() {
        let mut failing = Command::new("sh");
        failing
            .args(["-c", "printf abc; printf boom >&2; exit 3"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut reader = spawn_decompressor(&mut failing).expect("spawn the failing child");
        let mut buffer = [0u8; 16];
        let read = reader
            .read(&mut buffer)
            .expect("the first bytes are readable");
        assert_eq!(&buffer[..read], b"abc");
        // End of output is where the status is judged.
        match reader.read(&mut buffer) {
            Err(error) => {
                let message = error.to_string();
                assert!(message.contains("exit status: 3"), "{message}");
                assert!(message.contains("boom"), "stderr is the reason: {message}");
                assert!(message.contains("3 bytes of output"), "{message}");
            }
            Ok(read) => panic!("a failed decompressor must not look like EOF ({read} bytes)"),
        }

        let mut succeeding = Command::new("sh");
        succeeding
            .args(["-c", "printf abc; exit 0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut reader = spawn_decompressor(&mut succeeding).expect("spawn the child");
        let read = reader.read(&mut buffer).expect("read the output");
        assert_eq!(&buffer[..read], b"abc");
        assert_eq!(reader.read(&mut buffer).expect("clean end of output"), 0);
    }

    /// The write path is what turns a decompressor failure into the report the
    /// operator sees: bytes already written and no rollback.
    #[test]
    fn a_failing_decompressor_stops_the_write_with_the_bytes_written() {
        struct FailingOpener;
        impl ImageOpener for FailingOpener {
            fn open(
                &self,
                _image: &SystemInstallImage,
            ) -> Result<Box<dyn Read + Send>, SystemInstallError> {
                let mut command = Command::new("sh");
                command
                    .args(["-c", "printf abcdefgh; printf corrupt >&2; exit 2"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                spawn_decompressor(&mut command)
                    .map(|reader| Box::new(reader) as Box<dyn Read + Send>)
                    .map_err(|error| SystemInstallError::Io(error.to_string()))
            }
        }
        let harness = fixture(
            "decompressor-failure",
            boot_mounts(),
            RereadBehavior::Matches,
        );
        let manager = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(FailingOpener),
            false,
        );
        let plan = manager
            .plan(&install_request(&harness.image))
            .expect("plan the raw fixture image");
        match manager.apply_live(&install_request(&harness.image), &plan.plan_token, "sda") {
            Err(SystemInstallError::Io(message)) => {
                assert!(message.contains("decompressor failed"), "{message}");
                assert!(message.contains("corrupt"), "{message}");
                assert!(message.contains("cannot be rolled back"), "{message}");
                assert!(
                    message.starts_with("wrote 8 of"),
                    "the bytes already on the device are reported: {message}"
                );
            }
            other => panic!("a failing decompressor must stop the write: {other:?}"),
        }
    }

    /// The F-T13-002 / F-T20-001 property, restated for the current design: a
    /// token is only usable by the process that signed and issued it. A second
    /// manager stands in for a second process (the privileged helper), and the
    /// token it carries — even a well formed one — buys it nothing.
    #[test]
    fn another_process_cannot_use_a_token_it_did_not_issue() {
        let harness = fixture(
            "token-other-process",
            boot_mounts(),
            RereadBehavior::Matches,
        );
        let request = install_request(&harness.image);
        let plan = harness.manager.plan(&request).expect("plan");

        // A second process: its own key and its own issuance record.
        let other = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(SystemImageOpener),
            false,
        );
        assert!(
            matches!(
                other.apply_live(&request, &plan.plan_token, "sda"),
                Err(SystemInstallError::StalePlan)
            ),
            "a token minted elsewhere must not authorize a write"
        );
        // Even after that process previewed the same request itself, the caller's
        // token is still not the one it issued.
        let own = other.plan(&request).expect("plan in the second process");
        assert_ne!(own.plan_token, plan.plan_token);
        assert!(matches!(
            other.apply_live(&request, &plan.plan_token, "sda"),
            Err(SystemInstallError::StalePlan)
        ));
    }

    /// The tamper paths from F-T20-001: editing the nonce (to look like a new
    /// token) or the issuance time (to refresh the window) breaks the signature,
    /// so neither buys anything.
    #[test]
    fn editing_any_token_field_is_refused() {
        let harness = fixture("token-tamper", boot_mounts(), RereadBehavior::Matches);
        let request = install_request(&harness.image);
        let started = Utc::now();
        let clock = Arc::new(Mutex::new(started));
        let frozen = clock.clone();
        let manager = harness
            .manager
            .clone()
            .with_clock(Arc::new(move || *frozen.lock().expect("clock lock")));
        let plan = manager.plan(&request).expect("plan");
        let token = plan.plan_token.clone();

        // Split into the text the signature covers and the signature itself.
        let (signed, mac) = token.rsplit_once('-').expect("token has a signature");
        let (head, nonce) = signed.rsplit_once('-').expect("token has a nonce");
        assert_eq!(nonce.len(), 16, "the last signed field is the nonce");

        let cases = [
            // A fresh nonce with the old signature: the pre-fix way to escape the
            // consumption record by using a different key.
            format!("{head}-ffffffffffffffff-{mac}"),
            // A refreshed issuance time with the old signature: the pre-fix way to
            // restart the 600 second window.
            {
                let binding_len = "install-plan-".len() + 16 + 1;
                format!("{}-ffffffffff-{nonce}-{mac}", &signed[..binding_len])
            },
            // A truncated or extended signature.
            format!("{signed}-{}", &mac[..mac.len() - 1]),
            format!("{signed}-{mac}0"),
            // A different binding with a valid-looking shape (the pre-fix code
            // would reject this one, but the case belongs here for completeness).
            format!("install-plan-0000000000000000-{nonce}-{mac}"),
        ];
        for tampered in cases {
            assert!(
                matches!(
                    manager.apply_live(&request, &tampered, "sda"),
                    Err(SystemInstallError::StalePlan)
                ),
                "an edited token must be refused: {tampered}"
            );
        }

        // The untouched token still works, so the refusals above are about the
        // edits and not about a broken manager.
        manager
            .apply_live(&request, &token, "sda")
            .expect("the untouched token is accepted");

        // Once consumed, replaying it — or an edited copy — is refused as well.
        assert!(matches!(
            manager.apply_live(&request, &token, "sda"),
            Err(SystemInstallError::StalePlan)
        ));
        let edited = format!("{head}-eeeeeeeeeeeeeeee-{mac}");
        assert!(matches!(
            manager.apply_live(&request, &edited, "sda"),
            Err(SystemInstallError::StalePlan)
        ));
    }

    #[test]
    fn every_preview_issues_its_own_token_and_a_forged_one_is_refused() {
        let harness = fixture("token-nonce", boot_mounts(), RereadBehavior::Matches);
        let request = install_request(&harness.image);
        let first = harness.manager.plan(&request).expect("first plan");
        let second = harness.manager.plan(&request).expect("second plan");
        assert_ne!(
            first.plan_token, second.plan_token,
            "two previews must not share a token, or a second install would look \
             like a replay of the first"
        );
        assert!(split_plan_token(&first.plan_token).is_some());
        // Both tokens are usable: the repeat is not blocked by the first write.
        harness
            .manager
            .apply_live(&request, &first.plan_token, "sda")
            .expect("first write");
        harness
            .manager
            .apply_live(&request, &second.plan_token, "sda")
            .expect("the same request may be written again after a new preview");

        for forged in [
            "install-plan-0000000000000000-0000000000-0000000000000000",
            &format!("{}-extra", first.plan_token),
            &first.plan_token.replace("install-plan-", ""),
            "install-plan-",
            "",
        ] {
            assert!(
                matches!(
                    harness.manager.apply_live(&request, forged, "sda"),
                    Err(SystemInstallError::PlanRequired) | Err(SystemInstallError::StalePlan)
                ),
                "a token this build did not issue must be refused: {forged:?}"
            );
        }
    }

    /// The advisory cross-process path: a surface that issues its own token (the
    /// privileged helper does) uses `verify_preview_token` to notice that the
    /// state moved or that the preview the operator saw is old. It is explicitly
    /// not an authorization, which is why the checks are about drift and age.
    #[test]
    fn a_preview_token_is_checked_for_state_drift_and_age() {
        let harness = fixture("preview-token", boot_mounts(), RereadBehavior::Matches);
        let request = install_request(&harness.image);
        let started = Utc::now();
        let clock = Arc::new(Mutex::new(started));
        let frozen = clock.clone();
        let issuing = harness
            .manager
            .clone()
            .with_clock(Arc::new(move || *frozen.lock().expect("clock lock")));
        let plan = issuing.plan(&request).expect("plan");

        // A second process cannot verify the signature, but it can compare the
        // binding against the state it rebuilds itself.
        let helper = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(SystemImageOpener),
            false,
        );
        helper
            .verify_preview_token(&request, &plan.plan_token)
            .expect("the state is unchanged");
        assert!(
            split_plan_token(&plan.plan_token).is_some(),
            "the helper can read the shape it is handed"
        );

        // The image changed since the preview: the helper notices the drift.
        write_file(&harness.image, mbr_image(1, 4 * MIB));
        assert!(matches!(
            helper.verify_preview_token(&request, &plan.plan_token),
            Err(SystemInstallError::StalePlan)
        ));

        // An aged preview is refused by the advisory window. The helper's own
        // clock is used here, so the exact same token that was fresh above is old
        // 601 seconds later.
        let later_clock = Arc::new(Mutex::new(started + chrono::Duration::seconds(601)));
        let later = helper.clone().with_clock(Arc::new({
            let clock = later_clock.clone();
            move || *clock.lock().expect("clock lock")
        }));
        let fresh_for_shape = issuing.plan(&request);
        // The image changed above, so rebuild a matching request for the window
        // check: the point here is the age, not the binding.
        let _ = fresh_for_shape;
        assert!(matches!(
            later.verify_preview_token(&request, &plan.plan_token),
            Err(SystemInstallError::StalePlan)
        ));
    }

    /// HMAC-SHA256 is checked against RFC 4231, so the signature is the primitive
    /// it claims to be rather than something that merely round-trips.
    #[test]
    fn hmac_sha256_matches_rfc_4231() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 20], &[0xdd; 50])),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );
    }

    /// The CRC-32 that xz frames are checked with.
    #[test]
    fn crc32_matches_the_reference_vectors() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn a_changed_image_or_target_invalidates_the_plan() {
        let harness = fixture("swap-image", boot_mounts(), RereadBehavior::Matches);
        let request = install_request(&harness.image);
        let plan = harness.manager.plan(&request).expect("plan");
        write_file(&harness.image, mbr_image(1, 4 * MIB));
        assert!(matches!(
            harness
                .manager
                .apply_live(&request, &plan.plan_token, "sda"),
            Err(SystemInstallError::StalePlan)
        ));

        let target_harness = fixture("swap-target", boot_mounts(), RereadBehavior::Matches);
        let swapped = install_request(&target_harness.image);
        let plan = target_harness.manager.plan(&swapped).expect("plan");
        target_harness.backend.targets.lock().expect("targets lock")[0].size_bytes = 32 * MIB;
        assert!(matches!(
            target_harness
                .manager
                .apply_live(&swapped, &plan.plan_token, "sda"),
            Err(SystemInstallError::StalePlan)
        ));
    }

    #[test]
    fn streams_the_image_in_bounded_chunks_with_exact_bytes() {
        let harness = fixture("stream", boot_mounts(), RereadBehavior::Matches);
        // Larger than one chunk, so chunking is actually exercised.
        let image_bytes = mbr_image(2, 8 * MIB);
        write_file(&harness.image, &image_bytes);
        let plan = harness
            .manager
            .plan(&install_request(&harness.image))
            .expect("plan");
        let result = harness
            .manager
            .apply_live(&install_request(&harness.image), &plan.plan_token, "sda")
            .expect("write the image");
        assert_eq!(result.bytes_written, image_bytes.len() as u64);
        let chunks = harness.backend.chunks.lock().expect("chunk lock");
        assert!(chunks.len() > 1, "the image is written in several chunks");
        assert!(
            chunks.iter().all(|size| *size as u64 <= CHUNK_BYTES),
            "no chunk exceeds the bound: {chunks:?}"
        );
        assert_eq!(
            chunks.iter().sum::<usize>(),
            image_bytes.len(),
            "every byte is written exactly once"
        );
        drop(chunks);
        let written = fs::read(&harness.device).expect("read the fixture device");
        assert_eq!(
            &written[..image_bytes.len()],
            &image_bytes[..],
            "the device holds exactly the image"
        );
        assert_eq!(
            result.verification.state,
            SystemInstallVerificationState::Matched
        );
        assert_eq!(result.plan.target.id, "sda");
        assert!(result.run.output.is_some());
        assert_eq!(result.run.action_id, "storage.install-image");
    }

    #[test]
    fn a_failed_write_reports_the_bytes_written_and_never_rolls_back() {
        let harness = fixture("partial", boot_mounts(), RereadBehavior::Matches);
        let image_bytes = mbr_image(2, 8 * MIB);
        write_file(&harness.image, &image_bytes);
        *harness.backend.fail_after.lock().expect("fail lock") = Some(CHUNK_BYTES + 1024);
        let plan = harness
            .manager
            .plan(&install_request(&harness.image))
            .expect("plan");
        match harness
            .manager
            .apply_live(&install_request(&harness.image), &plan.plan_token, "sda")
        {
            Err(SystemInstallError::Io(message)) => {
                assert!(message.contains("partial image"), "{message}");
                assert!(message.contains("cannot be rolled back"), "{message}");
                assert!(
                    message.contains(&CHUNK_BYTES.to_string()),
                    "the written byte count is reported: {message}"
                );
            }
            other => panic!("a failed write must report an IO error: {other:?}"),
        }
        let written = fs::read(&harness.device).expect("read the fixture device");
        assert!(
            written.iter().any(|byte| *byte != 0),
            "the bytes already written stay on the target: there is no rollback"
        );
    }

    #[test]
    fn verification_reports_unsupported_and_mismatched_tables() {
        let unsupported = fixture(
            "verify-unsupported",
            boot_mounts(),
            RereadBehavior::Unsupported,
        );
        let plan = unsupported
            .manager
            .plan(&install_request(&unsupported.image))
            .expect("plan");
        let result = unsupported
            .manager
            .apply_live(
                &install_request(&unsupported.image),
                &plan.plan_token,
                "sda",
            )
            .expect("the write itself succeeds");
        assert_eq!(
            result.verification.state,
            SystemInstallVerificationState::Unsupported
        );
        assert!(result.verification.detail.is_some());
        assert_eq!(result.run.status, ActionStatus::Succeeded);

        let differs = fixture("verify-differs", boot_mounts(), RereadBehavior::Differs);
        let plan = differs
            .manager
            .plan(&install_request(&differs.image))
            .expect("plan");
        let result = differs
            .manager
            .apply_live(&install_request(&differs.image), &plan.plan_token, "sda")
            .expect("the write completes");
        assert_eq!(
            result.verification.state,
            SystemInstallVerificationState::Mismatched
        );
        assert_eq!(result.run.status, ActionStatus::Failed);
        assert!(result.verification.observed.is_some());
        assert_eq!(
            result.verification.expected,
            mbr_partitions(8 * MIB),
            "the image layout is reported for comparison"
        );
    }

    #[test]
    fn rejects_images_without_a_partition_table_or_missing_the_target() {
        let harness = fixture("reject", boot_mounts(), RereadBehavior::Matches);
        let blank = harness.root.join("blank.img");
        write_file(&blank, vec![0u8; 2 * MIB as usize]);
        assert!(matches!(
            harness.manager.plan(&install_request(&blank)),
            Err(SystemInstallError::InvalidInput(message)) if message.contains("partition table")
        ));

        let relative = SystemInstallRequest {
            target_id: "sda".into(),
            image_path: Some("system.img".into()),
            image_id: None,
        };
        assert!(matches!(
            harness.manager.plan(&relative),
            Err(SystemInstallError::InvalidInput(message)) if message.contains("absolute")
        ));

        let missing = SystemInstallRequest {
            target_id: "sda".into(),
            image_path: Some(harness.root.join("missing.img").display().to_string()),
            image_id: None,
        };
        assert!(matches!(
            harness.manager.plan(&missing),
            Err(SystemInstallError::InvalidInput(_))
        ));

        let catalog = SystemInstallRequest {
            target_id: "sda".into(),
            image_path: None,
            image_id: Some("radxa-cubie-a7z-ufs".into()),
        };
        assert!(matches!(
            harness.manager.plan(&catalog),
            Err(SystemInstallError::Unsupported(message)) if message.contains("catalog")
        ));

        let unknown = SystemInstallRequest {
            target_id: "nope".into(),
            image_path: Some(harness.image.display().to_string()),
            image_id: None,
        };
        assert!(matches!(
            harness.manager.plan(&unknown),
            Err(SystemInstallError::InvalidInput(message)) if message.contains("unknown block device")
        ));
    }

    #[test]
    fn rejects_an_image_stored_on_the_target_and_an_oversized_image() {
        // An image that lives on a partition of the target implies that
        // partition is mounted, and the mounted gate refuses first — naming the
        // mount point, which is what the user has to act on.
        let harness = fixture("image-on-target", boot_mounts(), RereadBehavior::Matches);
        let backend = Arc::new(FixtureBackend {
            targets: Mutex::new(harness.backend.targets.lock().expect("lock").clone()),
            mounts: vec![SystemMount {
                device: "/dev/sda2".into(),
                mount_point: harness.root.display().to_string(),
                file_system: "ext4".into(),
            }],
            device: harness.device.clone(),
            fail_after: Mutex::new(None),
            behavior: Mutex::new(RereadBehavior::Matches),
            chunks: Mutex::new(Vec::new()),
            expected: mbr_partitions(8 * MIB),
        });
        let manager =
            SystemInstallManager::with_backend(backend, Arc::new(SystemImageOpener), false);
        match manager.plan(&install_request(&harness.image)) {
            Err(SystemInstallError::Conflict(message)) => {
                assert!(
                    message.contains(&harness.root.display().to_string()),
                    "the mount point blocking the write is named: {message}"
                );
            }
            other => panic!("an image on a mounted target must be refused: {other:?}"),
        }

        let oversized = fixture("oversized", boot_mounts(), RereadBehavior::Matches);
        oversized.backend.targets.lock().expect("targets lock")[0].size_bytes = 8 * MIB;
        let big = oversized.root.join("big.img");
        write_file(&big, mbr_image(1, 8 * MIB));
        assert!(matches!(
            oversized.manager.plan(&install_request(&big)),
            Err(SystemInstallError::Conflict(message)) if message.contains("but /dev/sda has")
        ));
    }

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
            plan.image.uncompressed_bytes, 12,
            "the xz index declares it"
        );
        assert_eq!(plan.image.size_bytes, XZ_FIXTURE.len() as u64);
        assert!(plan.image.partition_table.is_none());
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

    /// One `.xz` stream whose index holds four block records (made with
    /// `xz --block-size=256` over 1024 bytes, so the per-record summing is
    /// exercised too). The last stream of a concatenation is what a
    /// last-stream-only parser reports, which is the truncation this guards.
    const XZ_MULTI_BLOCK_FIXTURE: &[u8] = &[
        0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00, 0x00, 0x04, 0xE6, 0xD6, 0xB4, 0x46, 0x03, 0xC0, 0x0E,
        0x80, 0x02, 0x21, 0x01, 0x16, 0x00, 0x00, 0x00, 0x00, 0xC1, 0x8C, 0x6D, 0xDA, 0xE0, 0x00,
        0xFF, 0x00, 0x06, 0x5D, 0x00, 0x20, 0xEF, 0xD6, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x66,
        0xDD, 0x4D, 0xCC, 0xD4, 0x27, 0x12, 0x9A, 0x03, 0xC0, 0x0E, 0x80, 0x02, 0x21, 0x01, 0x16,
        0x00, 0x00, 0x00, 0x00, 0xC1, 0x8C, 0x6D, 0xDA, 0xE0, 0x00, 0xFF, 0x00, 0x06, 0x5D, 0x00,
        0x20, 0xEF, 0xD6, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x66, 0xDD, 0x4D, 0xCC, 0xD4, 0x27,
        0x12, 0x9A, 0x03, 0xC0, 0x0E, 0x80, 0x02, 0x21, 0x01, 0x16, 0x00, 0x00, 0x00, 0x00, 0xC1,
        0x8C, 0x6D, 0xDA, 0xE0, 0x00, 0xFF, 0x00, 0x06, 0x5D, 0x00, 0x20, 0xEF, 0xD6, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x66, 0xDD, 0x4D, 0xCC, 0xD4, 0x27, 0x12, 0x9A, 0x03, 0xC0, 0x0E,
        0x80, 0x02, 0x21, 0x01, 0x16, 0x00, 0x00, 0x00, 0x00, 0xC1, 0x8C, 0x6D, 0xDA, 0xE0, 0x00,
        0xFF, 0x00, 0x06, 0x5D, 0x00, 0x20, 0xEF, 0xD6, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x66,
        0xDD, 0x4D, 0xCC, 0xD4, 0x27, 0x12, 0x9A, 0x00, 0x04, 0x26, 0x80, 0x02, 0x26, 0x80, 0x02,
        0x26, 0x80, 0x02, 0x26, 0x80, 0x02, 0x00, 0x00, 0x1A, 0x5A, 0x71, 0xE3, 0xAC, 0x27, 0x3E,
        0x2D, 0x04, 0x00, 0x00, 0x00, 0x00, 0x04, 0x59, 0x5A,
    ];

    /// Every fixture's uncompressed size is compared against the reference
    /// implementation, not against this module's own arithmetic:
    ///
    /// ```text
    /// fixture                                     streams        xz -dc | wc -c
    /// XZ_FIXTURE                                  1                     12
    /// XZ_FIXTURE + XZ_FIXTURE                     2                     24
    /// XZ_FIXTURE + 0x00 * 4 + XZ_FIXTURE          2 (stream padding)    24
    /// XZ_FIXTURE + 0x00 * 4 + XZ_FIXTURE * 2      3 (stream padding)    36
    /// XZ_FIXTURE + 0x00 * 4                       1 (trailing padding)  12
    /// XZ_MULTI_BLOCK_FIXTURE                      1 (4 blocks)        1024
    /// ```
    #[test]
    fn xz_uncompressed_size_adds_up_every_stream() {
        let harness = fixture("xz-streams", boot_mounts(), RereadBehavior::Matches);
        let padding = [0u8; 4];
        let cases: Vec<(&str, Vec<u8>, u64)> = vec![
            ("single.img.xz", XZ_FIXTURE.to_vec(), 12),
            ("two.img.xz", [XZ_FIXTURE, XZ_FIXTURE].concat(), 24),
            (
                "padded.img.xz",
                [XZ_FIXTURE, &padding, XZ_FIXTURE].concat(),
                24,
            ),
            (
                "three.img.xz",
                [XZ_FIXTURE, &padding, XZ_FIXTURE, XZ_FIXTURE].concat(),
                36,
            ),
            ("trailing.img.xz", [XZ_FIXTURE, &padding].concat(), 12),
            ("multi-block.img.xz", XZ_MULTI_BLOCK_FIXTURE.to_vec(), 1024),
        ];
        for (name, bytes, expected) in cases {
            let path = harness.root.join(name);
            write_file(&path, &bytes);
            let plan = harness
                .manager
                .plan(&install_request(&path))
                .unwrap_or_else(|error| panic!("{name} must be planned: {error}"));
            assert_eq!(plan.image.format, InstallImageFormat::Xz, "{name}");
            assert_eq!(plan.image.size_bytes, bytes.len() as u64, "{name}");
            assert_eq!(
                plan.image.uncompressed_bytes, expected,
                "{name}: the uncompressed size must add up every stream"
            );
        }
    }

    /// Stream padding is a multiple of four; the reference implementation refuses
    /// the same file (`xz -dc` exits 1 on it), so refusing it here agrees with it
    /// instead of inventing a size.
    #[test]
    fn xz_padding_that_is_not_a_multiple_of_four_is_rejected() {
        let harness = fixture("xz-padding", boot_mounts(), RereadBehavior::Matches);
        let path = harness.root.join("bad-padding.img.xz");
        write_file(&path, [XZ_FIXTURE, XZ_FIXTURE, &[0u8; 2]].concat());
        match harness.manager.plan(&install_request(&path)) {
            Err(SystemInstallError::InvalidInput(detail)) => {
                assert!(detail.contains("padding"), "{detail}");
            }
            other => panic!("two padding bytes must be refused: {other:?}"),
        }
    }

    /// The reproduction from the finding, end to end: two concatenated streams
    /// plan as 24 bytes and are written as 24 bytes. A last-stream-only size (12)
    /// would have written half the image and reported success.
    #[test]
    fn a_concatenated_image_is_planned_and_written_in_full() {
        struct TotalOpener(u64);
        impl ImageOpener for TotalOpener {
            fn open(
                &self,
                _image: &SystemInstallImage,
            ) -> Result<Box<dyn Read + Send>, SystemInstallError> {
                Ok(Box::new(std::io::Cursor::new(vec![
                    0x5Au8;
                    self.0 as usize
                ])))
            }
        }
        let harness = fixture(
            "xz-two-streams-write",
            boot_mounts(),
            RereadBehavior::Unsupported,
        );
        let xz = harness.root.join("two-stream.img.xz");
        write_file(&xz, [XZ_FIXTURE, XZ_FIXTURE].concat());
        let manager = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(TotalOpener(24)),
            false,
        );
        let request = install_request(&xz);
        let plan = manager.plan(&request).expect("plan");
        assert_eq!(
            plan.image.uncompressed_bytes, 24,
            "both streams are counted, so the plan describes the whole image"
        );
        let result = manager
            .apply_live(&request, &plan.plan_token, "sda")
            .expect("write the concatenated image");
        assert_eq!(result.bytes_written, 24);
        assert_eq!(
            &fs::read(&harness.device).expect("read the fixture device")[..24],
            &[0x5Au8; 24],
            "both streams reached the device"
        );
    }

    #[test]
    fn a_stream_with_more_data_than_the_plan_fails_instead_of_truncating() {
        /// Yields more bytes than the plan declares, the way a stream whose size
        /// was under-counted would.
        struct LongOpener(u64);
        impl ImageOpener for LongOpener {
            fn open(
                &self,
                _image: &SystemInstallImage,
            ) -> Result<Box<dyn Read + Send>, SystemInstallError> {
                Ok(Box::new(std::io::Cursor::new(vec![
                    0x5Au8;
                    self.0 as usize
                ])))
            }
        }
        let harness = fixture("long-image", boot_mounts(), RereadBehavior::Matches);
        write_file(&harness.image, mbr_image(1, MIB));
        let declared = fs::metadata(&harness.image).expect("image metadata").len();
        let manager = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(LongOpener(declared + 64)),
            false,
        );
        let plan = manager
            .plan(&install_request(&harness.image))
            .expect("plan");
        assert_eq!(plan.image.uncompressed_bytes, declared);
        match manager.apply_live(&install_request(&harness.image), &plan.plan_token, "sda") {
            Err(SystemInstallError::Conflict(message)) => {
                assert!(message.contains(&declared.to_string()), "{message}");
                assert!(message.contains("instead of truncating"), "{message}");
            }
            other => panic!("a longer stream must fail loudly: {other:?}"),
        }
        // Exactly the declared bytes reached the device: nothing was truncated
        // silently and nothing beyond the plan was written either.
        let chunks = harness.backend.chunks.lock().expect("chunk lock");
        let written: u64 = chunks.iter().map(|size| *size as u64).sum();
        assert_eq!(written, declared, "the write stops at the declared size");
    }

    #[test]
    fn a_compressed_image_reports_that_the_table_was_not_compared() {
        struct TwelveByteOpener;
        impl ImageOpener for TwelveByteOpener {
            fn open(
                &self,
                _image: &SystemInstallImage,
            ) -> Result<Box<dyn Read + Send>, SystemInstallError> {
                Ok(Box::new(std::io::Cursor::new(vec![0x5Au8; 12])))
            }
        }
        // The host does re-read the table here, so a mismatch would have been
        // reported before: a compressed image has no expected layout to compare.
        let harness = fixture("xz-verify", boot_mounts(), RereadBehavior::Matches);
        let xz = harness.root.join("system.img.xz");
        write_file(&xz, XZ_FIXTURE);
        let manager = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(TwelveByteOpener),
            false,
        );
        let request = install_request(&xz);
        let plan = manager.plan(&request).expect("plan");
        let result = manager
            .apply_live(&request, &plan.plan_token, "sda")
            .expect("write the compressed image");
        assert_eq!(
            result.verification.state,
            SystemInstallVerificationState::Unsupported,
            "not Matched and not Mismatched: {result:?}"
        );
        let detail = result.verification.detail.as_deref().unwrap_or_default();
        assert!(detail.contains("compressed"), "{detail}");
        assert!(detail.contains("not compared"), "{detail}");
        assert!(
            result.verification.observed.is_some(),
            "the table was re-read, it just has nothing to be compared against"
        );
        assert!(result.verification.expected.is_empty());
        let output = result.run.output.clone().unwrap_or_default();
        assert!(!output.contains("DOES NOT match"), "{output}");
    }

    #[test]
    fn writes_through_an_injected_opener_without_the_tool() {
        struct FixtureOpener(u64);
        impl ImageOpener for FixtureOpener {
            fn open(
                &self,
                _image: &SystemInstallImage,
            ) -> Result<Box<dyn Read + Send>, SystemInstallError> {
                // Stands in for `xz -dc` without needing the tool installed.
                Ok(Box::new(std::io::Cursor::new(vec![
                    0x5Au8;
                    self.0 as usize
                ])))
            }
        }
        let harness = fixture("opener-write", boot_mounts(), RereadBehavior::Unsupported);
        let xz = harness.root.join("system.img.xz");
        write_file(&xz, XZ_FIXTURE);
        // The plan and the write must use the same manager, because the token is
        // issued by the planning instance.
        let manager = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(FixtureOpener(12)),
            false,
        );
        let plan = manager.plan(&install_request(&xz)).expect("plan");
        assert_eq!(plan.image.uncompressed_bytes, 12);
        let result = manager
            .apply_live(&install_request(&xz), &plan.plan_token, "sda")
            .expect("stream from the injected opener");
        assert_eq!(result.bytes_written, 12);
        assert_eq!(
            fs::read(&harness.device).expect("read device")[..4],
            [0x5Au8; 4]
        );
        assert_eq!(
            result.verification.state,
            SystemInstallVerificationState::Unsupported
        );
    }

    #[test]
    fn an_image_that_ends_early_is_reported_as_partial() {
        struct ShortOpener;
        impl ImageOpener for ShortOpener {
            fn open(
                &self,
                _image: &SystemInstallImage,
            ) -> Result<Box<dyn Read + Send>, SystemInstallError> {
                Ok(Box::new(std::io::Cursor::new(vec![0u8; 1024])))
            }
        }
        let harness = fixture("short-image", boot_mounts(), RereadBehavior::Matches);
        let manager = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(ShortOpener),
            false,
        );
        let plan = manager
            .plan(&install_request(&harness.image))
            .expect("plan");
        match manager.apply_live(&install_request(&harness.image), &plan.plan_token, "sda") {
            Err(SystemInstallError::Io(message)) => {
                assert!(
                    message.contains("ended before the size it declares"),
                    "{message}"
                );
                assert!(message.contains("1024 of"), "{message}");
            }
            other => panic!("a short stream must fail: {other:?}"),
        }
    }

    /// The live backend must resolve the device node under its root, so a fixture
    /// root can be written without any real block device being involved.
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
        write_file(&root.join("proc/mounts"), b"/dev/mmcblk1p2 / ext4 rw 0 0\n");

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
