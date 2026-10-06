//! Controlled Linux child boundary for the three inherited descriptors.
use std::{
    fs::{self, File},
    io::Read,
    os::fd::{FromRawFd, OwnedFd},
};

use super::AuthorizedRun;

const MAX_FDINFO: u64 = 4096;
// Linux fcntl.h: O_ACCMODE=03, O_PATH=010000000; avoid a new libc dependency.
const O_ACCMODE: u32 = 0o3;
const O_PATH: u32 = 0o10000000;

pub(crate) struct InheritedChildFds {
    pub(crate) writer: OwnedFd,
    pub(crate) observer: OwnedFd,
    pub(crate) artifact: OwnedFd,
}

fn read_only_regular(fd: i32) -> bool {
    let Ok(metadata) = fs::metadata(format!("/proc/self/fd/{fd}")) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    let Ok(mut file) = File::open(format!("/proc/self/fdinfo/{fd}")) else {
        return false;
    };
    let mut info = Vec::new();
    if file
        .by_ref()
        .take(MAX_FDINFO + 1)
        .read_to_end(&mut info)
        .is_err()
        || info.len() as u64 > MAX_FDINFO
    {
        return false;
    }
    let Ok(info) = std::str::from_utf8(&info) else {
        return false;
    };
    let mut lines = info
        .lines()
        .filter_map(|line| line.strip_prefix("flags:\t"));
    let Some(value) = lines.next() else {
        return false;
    };
    if lines.next().is_some()
        || value.is_empty()
        || !value.bytes().all(|b| (b'0'..=b'7').contains(&b))
    {
        return false;
    }
    matches!(u32::from_str_radix(value, 8), Ok(flags) if flags & (O_ACCMODE | O_PATH) == 0)
}

/// Called only in the controlled single-owner child after parsing its authorization.
/// Validation failures terminate the process: no caller can continue to a spawn/connect.
///
/// # Safety
/// Every currently open descriptor in `raw` with number >= 3 must have been
/// exclusively handed off to this controlled child for this call. No other
/// `File`/`OwnedFd` may own one, and no concurrent close, dup, or reuse may
/// occur while this function checks and adopts the descriptors. A repeated
/// number denotes only one owner and is rejected before adoption. Numbers 0–2
/// and already-closed numbers are rejected without taking ownership. After
/// successful adoption, the caller must not take ownership again from any of
/// the raw numbers. `/proc` only checks current openness and descriptor type;
/// it cannot establish exclusive ownership or the absence of races.
pub(crate) unsafe fn adopt_child_or_exit(
    raw: [i32; 3],
    auth: &AuthorizedRun,
    env_pins: (&str, &str),
) -> InheritedChildFds {
    let pins = auth.pins();
    if pins != env_pins
        || raw.iter().any(|fd| *fd < 3)
        || raw[0] == raw[1]
        || raw[0] == raw[2]
        || raw[1] == raw[2]
        || !raw.iter().copied().all(read_only_regular)
    {
        // No raw fd was adopted. OS process teardown reclaims inherited descriptors;
        // this is not the OwnedFd loader's return-path cleanup.
        std::process::exit(1);
    }
    // SAFETY: This is the sole raw adoption boundary. The controlled child must
    // exclusively own all three valid inherited descriptors, with no other owner
    // and no concurrent close/reuse between checks and adoption. /proc validates
    // type/access, NOT exclusive ownership. This precondition is established by
    // the supervised parent/child handoff, not by these metadata checks.
    unsafe {
        InheritedChildFds {
            writer: OwnedFd::from_raw_fd(raw[0]),
            observer: OwnedFd::from_raw_fd(raw[1]),
            artifact: OwnedFd::from_raw_fd(raw[2]),
        }
    }
}
