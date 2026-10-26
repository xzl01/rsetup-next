//! Shared primitives for the hardware module tree.
use super::HardwareError;
use crate::{ActionRun, ActionStatus};
use chrono::{DateTime, Utc};
use std::{fs, path::Path};
use uuid::Uuid;

pub(super) fn with_rollback_result(
    error: HardwareError,
    rollback: Result<(), HardwareError>,
) -> HardwareError {
    match rollback {
        Ok(()) => error,
        Err(rollback) => HardwareError::Io(format!("{error}; rollback failed: {rollback}")),
    }
}

pub(crate) fn fingerprint(prefix: &str, bytes: &[u8]) -> String {
    let mut left = 0xcbf2_9ce4_8422_2325u64;
    let mut right = 0x8422_2325_cbf2_9ce4u64;
    for byte in bytes {
        left ^= u64::from(*byte);
        left = left.wrapping_mul(0x100_0000_01b3);
        right ^= u64::from(*byte).rotate_left(1);
        right = right.wrapping_mul(0x100_0000_01b3).rotate_left(7);
    }
    format!("{prefix}-{left:016x}{right:016x}")
}

pub(crate) fn display_path(root: &Path, path: &Path) -> String {
    if root == Path::new("/") {
        return path.display().to_string();
    }
    path.strip_prefix(root)
        .map(|path| format!("/{}", path.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

pub(super) fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
}

pub(super) fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(program).is_file())
    })
}

pub(crate) fn hardware_run(
    action_id: &str,
    title: &str,
    status: ActionStatus,
    synthetic: bool,
    summary: &str,
    output: Option<String>,
    started_at: DateTime<Utc>,
) -> ActionRun {
    ActionRun {
        id: Uuid::new_v4().to_string(),
        action_id: action_id.into(),
        action_title: title.into(),
        status,
        synthetic,
        summary: summary.into(),
        output,
        started_at,
        finished_at: Some(Utc::now()),
    }
}
