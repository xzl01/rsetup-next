//! The plan/apply state machine: issuing plan tokens, the hard order of
//! `apply_live` (target lock, rebuilt plan, token checks, typed
//! acknowledgement, single-use consumption, the bounded chunked write, sync,
//! re-read verification and the recorded run), and the privilege rule every
//! surface shares.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::model::{
    ActionRun, ActionStatus, InstallImageFormat, SystemInstallApplyResult, SystemInstallImage,
    SystemInstallPartition, SystemInstallPlan, SystemInstallRequest, SystemInstallTarget,
    SystemInstallVerification, SystemInstallVerificationState,
};

use super::backend_synth::SyntheticBackend;
use super::backend_sys::SystemBlockBackend;
use super::crypto::{constant_time_eq, hex, hmac_sha256, sha256};
use super::error::SystemInstallError;
use super::image::{file_identity, read_partition_table, same_partition_layout};
use super::mounts::{annotate_mounts, longest_mount_prefix, resolves_to_target};
use super::opener::SystemImageOpener;
use super::token::{
    ParsedToken, mint_nonce, mint_token_key, plan_token, revision, split_plan_token, token_binding,
};
use super::xz::{XZ_MAGIC, read_xz_size};
use super::{BlockDeviceBackend, IMAGE_PREFIX_BYTES, ImageOpener, PLAN_TTL, SystemMount};

/// One chunk of the image is held in memory at a time.
const CHUNK_BYTES: u64 = 4 * 1024 * 1024;

/// How far a token may be dated in the future before it is refused. Tokens are
/// minted from the local clock, so a small skew is normal and a large one is a
/// forged or broken token.
const TOKEN_CLOCK_SKEW: chrono::Duration = chrono::Duration::seconds(60);

/// Action identity used for the recorded run.
const INSTALL_ACTION_ID: &str = "storage.install-image";
const INSTALL_ACTION_TITLE: &str = "Install a system image";

/// Whether a real write has to be delegated to the fixed-verb helper.
///
/// The demo provider writes to nothing, and root opens the device itself;
/// anything else is refused by the kernel when it opens the target, so routing it
/// through `pkexec` is what turns a `PermissionDenied` into an administrator
/// prompt. Every surface shares this one rule instead of restating it.
pub fn install_needs_privilege(live: bool, synthetic: bool, effective_uid: Option<u32>) -> bool {
    live && !synthetic && effective_uid != Some(0)
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
    pub(super) fn with_clock(mut self, now: Clock) -> Self {
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
        // One writer per target for the whole sequence. The lock is taken before
        // the plan is rebuilt, so the gates are judged against the state left by
        // any previous writer, and it is held across verify, write, flush and
        // re-read. Without it two tokens for the same device are both accepted and
        // their chunks interleave, which leaves neither image on the device.
        let _target_lock = self.acquire_target_lock(&request.target_id)?;
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

        let dry_run = self.backend.is_dry_run();
        let mut summary = if dry_run {
            format!(
                "Planned {} bytes for {}; nothing was written (dry run).",
                plan.image.uncompressed_bytes, plan.target.path
            )
        } else {
            format!(
                "Wrote {} of {} bytes to {}.",
                bytes_written, plan.image.uncompressed_bytes, plan.target.path
            )
        };
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
            // A dry run is planned, not executed: saying "succeeded" would claim a
            // write that never happened.
            status: if dry_run {
                ActionStatus::Planned
            } else if verification.state == SystemInstallVerificationState::Mismatched {
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
            dry_run,
            verification,
        })
    }

    /// Takes the cross-process write lock for one target.
    ///
    /// The lock file name is derived from a caller supplied id, so only the
    /// characters a kernel block device name can contain are allowed through; the
    /// name is a single path component because separators are refused.
    fn acquire_target_lock(
        &self,
        target_id: &str,
    ) -> Result<Option<crate::transaction::ProcessLock>, SystemInstallError> {
        let Some(root) = self.backend.lock_root() else {
            return Ok(None);
        };
        if target_id.is_empty()
            || !target_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(SystemInstallError::InvalidInput(format!(
                "invalid target device identifier {target_id}"
            )));
        }
        crate::transaction::ProcessLock::acquire(
            &root,
            &format!("rsetup-next-install-{target_id}.lock"),
        )
        .map(Some)
        .map_err(|error| {
            SystemInstallError::Conflict(format!(
                "another write to {target_id} is already running: {error}"
            ))
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
        if let Some(mount) = longest_mount_prefix(mounts, Path::new(path))
            && resolves_to_target(&mount.device, target)
        {
            return Err(SystemInstallError::Conflict(format!(
                "the image {path} is stored on {} which is being overwritten",
                mount.mount_point
            )));
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
        // Read from the handle that was just opened, not from the path again: the
        // plan must describe the bytes this file holds now, and a path-based
        // second look could describe a different file.
        let handle = file.metadata().map_err(|error| {
            SystemInstallError::Io(format!("unable to stat the image header: {error}"))
        })?;
        let file_identity = file_identity(&handle);
        if header.starts_with(&XZ_MAGIC) {
            let uncompressed = read_xz_size(&mut file)?;
            let mut image = SystemInstallImage {
                path: path.to_owned(),
                id: request.image_id.clone(),
                format: InstallImageFormat::Xz,
                size_bytes,
                uncompressed_bytes: uncompressed,
                file_identity: file_identity.clone(),
                sha256_prefix,
                prefix_bytes: header.len() as u64,
                partition_table: None,
                partitions: Vec::new(),
            };
            // A valid xz container is not evidence that the payload is a disk
            // image: a compressed text file is a valid container too. Parse the
            // decompressed header before a plan can exist, so a payload that
            // would overwrite the target with something unbootable is refused
            // instead of being written and only then found unverifiable.
            let (table, partitions) = self.decompressed_partition_table(&image)?;
            image.partition_table = Some(table);
            image.partitions = partitions;
            return Ok(image);
        }
        let (table, partitions) = read_partition_table(&mut file)?;
        Ok(SystemInstallImage {
            path: path.to_owned(),
            id: request.image_id.clone(),
            format: InstallImageFormat::Raw,
            size_bytes,
            uncompressed_bytes: size_bytes,
            file_identity,
            sha256_prefix,
            prefix_bytes: header.len() as u64,
            partition_table: Some(table),
            partitions,
        })
    }

    /// Parses the partition table of a compressed image from a bounded prefix of
    /// its decompressed content.
    ///
    /// The prefix is capped at [`IMAGE_PREFIX_BYTES`], which covers the MBR, the
    /// GPT header and the whole 128 entry table the parser reads even at the
    /// largest entry size it accepts. Only a prefix is decompressed, so a
    /// multi-gigabyte image is not decompressed twice; the whole stream is still
    /// validated by the decompressor's exit status while writing.
    fn decompressed_partition_table(
        &self,
        image: &SystemInstallImage,
    ) -> Result<(String, Vec<SystemInstallPartition>), SystemInstallError> {
        let mut stream = self.opener.open(image)?;
        let mut prefix = vec![0u8; image.uncompressed_bytes.min(IMAGE_PREFIX_BYTES) as usize];
        let mut filled = 0usize;
        while filled < prefix.len() {
            match stream.read(&mut prefix[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) => {
                    return Err(SystemInstallError::InvalidInput(format!(
                        "the compressed image {} could not be decompressed to read its partition table: {error}",
                        image.path
                    )));
                }
            }
        }
        prefix.truncate(filled);
        read_partition_table(&mut Cursor::new(prefix))
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
            let want = chunk_len(buffer.len(), limit - written);
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
                // The re-read itself failed, so nothing was compared at all.
                // Claiming the table "does not match" would assert something the
                // observation cannot support — a kernel refusing BLKRRPART with
                // EINVAL on an image that was in fact written in full is exactly
                // the case that exposed this. The write is already done (this runs
                // after the sync), which the detail says too.
                state: SystemInstallVerificationState::Unsupported,
                expected: plan.image.partitions.clone(),
                observed: None,
                detail: Some(format!(
                    "the kernel could not re-read the partition table of {} after the image was written, so the write could not be verified: {error}",
                    plan.target.path
                )),
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

/// Bytes to ask the image stream for next.
///
/// The remainder is compared in `u64` and narrowed only afterwards. Narrowing
/// first truncated at 4 GiB on a 32-bit target (ARMv7, i686): an exact multiple
/// of 4 GiB became zero, which read no bytes and was reported as "the image
/// ended before the size it declares", and a remainder just over 4 GiB became a
/// tiny chunk that wrote a partial image before failing.
fn chunk_len(buffer_len: usize, remaining: u64) -> usize {
    (buffer_len as u64).min(remaining) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BlockDeviceKind;
    use crate::system_install::opener::spawn_decompressor;
    use crate::system_install::test_support::*;
    use crate::system_install::{GIB, MIB};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    #[test]
    fn chunk_length_is_narrowed_only_after_the_comparison() {
        let buffer = CHUNK_BYTES as usize;
        // Exactly 4 GiB: narrowing the remainder first produced a zero-length
        // read, which the write loop reported as a short image.
        assert_eq!(chunk_len(buffer, u64::from(u32::MAX) + 1), buffer);
        assert_eq!(chunk_len(buffer, 4 * GIB + 1), buffer);
        assert_eq!(chunk_len(buffer, 8 * GIB), buffer);
        // Small remainders still bound the read.
        assert_eq!(chunk_len(buffer, buffer as u64 - 1), buffer - 1);
        assert_eq!(chunk_len(buffer, 10), 10);
        assert_eq!(chunk_len(buffer, 0), 0);
    }

    /// Two plans naming the same target cannot both enter the write sequence. The
    /// lock is held across verify, write, flush and re-read, so chunks from two
    /// different images can never interleave on one device; without it both tokens
    /// are accepted and the device ends up holding neither image.
    #[test]
    fn a_second_write_to_the_same_target_is_refused_while_one_is_held() {
        let harness = fixture("install-lock", boot_mounts(), RereadBehavior::Matches);
        let lock_root = harness
            .backend
            .lock_root()
            .expect("a fixture root is lockable");
        let held =
            crate::transaction::ProcessLock::acquire(&lock_root, "rsetup-next-install-sda.lock")
                .expect("hold the target lock");
        let request = install_request(&harness.image);
        let plan = harness.manager.plan(&request).expect("plan");
        match harness
            .manager
            .apply_live(&request, &plan.plan_token, "sda")
        {
            Err(SystemInstallError::Conflict(message)) => {
                assert!(message.contains("already running"), "{message}");
            }
            other => panic!("a concurrent write must be refused: {other:?}"),
        }
        // The refused attempt never consumed the token, so the reviewed plan is
        // still usable once the other writer finishes.
        drop(held);
        assert!(
            harness
                .manager
                .apply_live(&request, &plan.plan_token, "sda")
                .is_ok(),
            "the plan survives a refused concurrent write"
        );
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

    /// The reproduction from the finding: the token only digested the first
    /// `IMAGE_PREFIX_BYTES`, so editing data beyond that while keeping the length
    /// left an old token valid and the plan would write bytes nobody reviewed.
    #[test]
    fn an_edit_past_the_hashed_prefix_invalidates_the_plan() {
        let harness = fixture("edit-past-prefix", boot_mounts(), RereadBehavior::Matches);
        let mut bytes = mbr_image(2, 8 * MIB);
        assert!(bytes.len() as u64 > IMAGE_PREFIX_BYTES * 3 / 2);
        write_file(&harness.image, &bytes);
        let request = install_request(&harness.image);
        let plan = harness.manager.plan(&request).expect("plan");

        // Change a byte at 1.5 MiB, past the 1 MiB the digest covers, and keep the
        // length identical. The modification time moves, so the binding must too.
        let offset = (IMAGE_PREFIX_BYTES + IMAGE_PREFIX_BYTES / 2) as usize;
        bytes[offset] ^= 0xFF;
        write_file(&harness.image, &bytes);
        assert_eq!(
            fs::metadata(&harness.image).expect("stat").len(),
            plan.image.size_bytes,
            "the length is unchanged, so only identity can catch this"
        );
        assert!(matches!(
            harness
                .manager
                .apply_live(&request, &plan.plan_token, "sda"),
            Err(SystemInstallError::StalePlan)
        ));

        // Re-planning the edited file is accepted: the gate is drift, not the edit.
        let fresh = harness.manager.plan(&request).expect("re-plan");
        assert_ne!(fresh.plan_token, plan.plan_token);
        assert!(
            harness
                .manager
                .apply_live(&request, &fresh.plan_token, "sda")
                .is_ok()
        );
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

    /// The board's case: the kernel refused the re-read itself (EINVAL) on a
    /// device whose image had just been written in full. That is "could not be
    /// verified", never "does not match", and the two must stay distinguishable.
    #[test]
    fn a_failed_reread_is_unverified_and_not_a_mismatch() {
        let harness = fixture("verify-reread-fails", boot_mounts(), RereadBehavior::Fails);
        let image_bytes = mbr_image(2, 8 * MIB);
        write_file(&harness.image, &image_bytes);
        let plan = harness
            .manager
            .plan(&install_request(&harness.image))
            .expect("plan");
        let result = harness
            .manager
            .apply_live(&install_request(&harness.image), &plan.plan_token, "sda")
            .expect("the write itself succeeds");

        assert_eq!(
            result.verification.state,
            SystemInstallVerificationState::Unsupported,
            "a failed re-read must not be reported as a mismatch: {result:?}"
        );
        assert!(result.verification.observed.is_none());
        let detail = result.verification.detail.as_deref().unwrap_or_default();
        assert!(detail.contains("could not re-read"), "{detail}");
        assert!(detail.contains("/dev/sda"), "{detail}");
        assert!(
            detail.contains("Invalid argument (os error 22)"),
            "the kernel's own reason is reported: {detail}"
        );
        assert!(
            detail.contains("after the image was written"),
            "the write completed and the detail says so: {detail}"
        );
        assert!(detail.contains("could not be verified"), "{detail}");
        assert!(
            !detail.contains("differs from the image"),
            "the mismatch wording must not be reused here: {detail}"
        );

        // The write really did finish: the device holds the image.
        assert_eq!(result.bytes_written, image_bytes.len() as u64);
        assert_eq!(
            &fs::read(&harness.device).expect("read the fixture device")[..image_bytes.len()],
            &image_bytes[..],
            "the image reached the device"
        );
        // And the run is a success, not the failure a mismatch reports.
        assert_eq!(result.run.status, ActionStatus::Succeeded);
        let output = result.run.output.clone().unwrap_or_default();
        assert!(!output.contains("DOES NOT match"), "{output}");

        // The genuine mismatch — the table was read and differs — keeps its own
        // state, status and wording, so the two cases cannot be conflated.
        let differs = fixture(
            "verify-reread-differs",
            boot_mounts(),
            RereadBehavior::Differs,
        );
        let plan = differs
            .manager
            .plan(&install_request(&differs.image))
            .expect("plan");
        let mismatched = differs
            .manager
            .apply_live(&install_request(&differs.image), &plan.plan_token, "sda")
            .expect("the write completes");
        assert_eq!(
            mismatched.verification.state,
            SystemInstallVerificationState::Mismatched
        );
        assert_eq!(mismatched.run.status, ActionStatus::Failed);
        assert!(mismatched.verification.observed.is_some());
        assert_ne!(result.verification.state, mismatched.verification.state);
        assert_ne!(result.verification.detail, mismatched.verification.detail);
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
    fn writes_through_an_injected_opener_without_the_tool() {
        struct FixtureOpener(u64);
        impl ImageOpener for FixtureOpener {
            fn open(
                &self,
                _image: &SystemInstallImage,
            ) -> Result<Box<dyn Read + Send>, SystemInstallError> {
                // Stands in for `xz -dc` without needing the tool installed. The
                // payload must be a parseable disk image, because a compressed
                // image's header is read while planning.
                Ok(Box::new(std::io::Cursor::new(mbr_bytes(self.0 as usize))))
            }
        }
        let harness = fixture("opener-write", boot_mounts(), RereadBehavior::Unsupported);
        let xz = harness.root.join("system.img.xz");
        write_file(&xz, XZ_FIXTURE);
        // The plan and the write must use the same manager, because the token is
        // issued by the planning instance.
        let manager = SystemInstallManager::with_backend(
            harness.backend.clone(),
            Arc::new(FixtureOpener(XZ_FIXTURE_BYTES)),
            false,
        );
        let plan = manager.plan(&install_request(&xz)).expect("plan");
        assert_eq!(plan.image.uncompressed_bytes, XZ_FIXTURE_BYTES);
        let result = manager
            .apply_live(&install_request(&xz), &plan.plan_token, "sda")
            .expect("stream from the injected opener");
        assert_eq!(result.bytes_written, XZ_FIXTURE_BYTES);
        let device = fs::read(&harness.device).expect("read device");
        assert_eq!(
            &device[510..512],
            &[0x55, 0xAA],
            "the MBR reached the device"
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
    /// A backend that reads a real target but never writes, like the CLI's
    /// `--live-execution`-less wrapper.
    struct DryRunFixture {
        inner: Arc<FixtureBackend>,
        counted: Mutex<u64>,
    }

    impl BlockDeviceBackend for DryRunFixture {
        fn targets(&self) -> Result<Vec<SystemInstallTarget>, SystemInstallError> {
            self.inner.targets()
        }

        fn mounts(&self) -> Result<Vec<SystemMount>, SystemInstallError> {
            self.inner.mounts()
        }

        fn write_chunk(
            &self,
            _target: &SystemInstallTarget,
            _offset: u64,
            chunk: &[u8],
        ) -> Result<(), SystemInstallError> {
            *self.counted.lock().expect("dry-run counter") += chunk.len() as u64;
            Ok(())
        }

        fn reread_partitions(
            &self,
            _target: &SystemInstallTarget,
        ) -> Result<Vec<SystemInstallPartition>, SystemInstallError> {
            Err(SystemInstallError::Unsupported(
                "dry run: the partition table was not re-read".into(),
            ))
        }

        fn lock_root(&self) -> Option<PathBuf> {
            self.inner.lock_root()
        }

        fn is_dry_run(&self) -> bool {
            true
        }
    }

    /// A preview must not come back as a completed write. The CLI's dry run
    /// reports a real target and counts the bytes, so without this the result said
    /// "Wrote 512 of 512 bytes" and a script had no way to tell that nothing had
    /// been written.
    #[test]
    fn a_dry_run_is_reported_as_planned_rather_than_written() {
        // Nothing is mounted, so the target is writable and the only difference
        // between the two runs is whether the backend writes.
        let fixture = fixture("dry-run-reporting", Vec::new(), RereadBehavior::Unsupported);
        let request = SystemInstallRequest {
            target_id: "sda".into(),
            image_path: Some(fixture.image.display().to_string()),
            image_id: None,
        };

        let dry = SystemInstallManager::with_backend(
            Arc::new(DryRunFixture {
                inner: fixture.backend.clone(),
                counted: Mutex::new(0),
            }),
            Arc::new(SystemImageOpener),
            // The target is real; only the write is withheld.
            false,
        );
        let plan = dry.plan(&request).expect("plan");
        let result = dry
            .apply_live(&request, &plan.plan_token, "sda")
            .expect("a dry run applies");

        assert!(result.dry_run, "the result must say it was a dry run");
        assert!(!result.run.synthetic, "the target is real, not synthetic");
        assert_eq!(result.run.status, ActionStatus::Planned);
        assert!(
            !result.run.summary.contains("Wrote"),
            "a dry run must not claim a write: {}",
            result.run.summary
        );
        assert!(
            result.run.summary.contains("nothing was written"),
            "{}",
            result.run.summary
        );

        // The same plan through a writing backend still reports a write.
        let plan = fixture.manager.plan(&request).expect("plan");
        let result = fixture
            .manager
            .apply_live(&request, &plan.plan_token, "sda")
            .expect("a real write applies");
        assert!(!result.dry_run);
        assert_eq!(result.run.status, ActionStatus::Succeeded);
        assert!(
            result.run.summary.contains("Wrote"),
            "{}",
            result.run.summary
        );
    }
}
