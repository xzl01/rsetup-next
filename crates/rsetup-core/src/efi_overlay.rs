//! Native Radxa EDK2/BLS overlay backend (Q6A, Q8B and the same installed layout).
//! Behaviour reference: radxa-pkg/rsetup's cli/edk2-menu.sh (GPL-3.0-or-later).
//! No shell sourcing, rsetup invocation, firmware writes or boot-default changes.
use crate::{
    ActionStatus,
    hardware::{
        HardwareError, OverlayApplyResult, OverlayBootChange, OverlayBootConfig, OverlayPlan,
        OverlayStatus, display_path, fingerprint, hardware_run, read_overlays,
    },
};
use chrono::Utc;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

const LIMIT: u64 = 32 * 1024 * 1024;
const AUTH_REASON: &str = "EFI boot files require administrator authorization to read.";

pub(crate) fn status(root: &Path, bootloader: &str) -> OverlayStatus {
    match Context::load(root) {
        Ok(context) => context.status(),
        Err(error) => {
            let authorization = matches!(error, HardwareError::RootRequired);
            OverlayStatus {
                collected_at: Utc::now(),
                synthetic: false,
                // A locked tool must stay reachable so the user can authorize a read.
                supported: authorization,
                mutable: false,
                bootloader: bootloader.into(),
                configuration_known: false,
                requires_authorization: authorization,
                cached: false,
                boot_entry: None,
                directory: None,
                revision: String::new(),
                overlays: Vec::new(),
                unavailable_reason: Some(if authorization {
                    AUTH_REASON.into()
                } else {
                    error.to_string()
                }),
            }
        }
    }
}

pub(crate) fn boot_change(
    status: &OverlayStatus,
    selected: &[String],
) -> Result<Option<OverlayBootChange>, HardwareError> {
    let Some(entry) = &status.boot_entry else {
        return Ok(None);
    };
    let directory = status
        .directory
        .as_deref()
        .ok_or_else(|| invalid("missing EFI overlay directory"))?;
    // Paths in BLS are relative to the ESP, not the Linux mount point.
    let relative = directory
        .strip_prefix("/boot/efi")
        .ok_or_else(|| invalid("unexpected EFI mount"))?;
    let requested = selected
        .iter()
        .map(|id| format!("{relative}/{id}"))
        .collect::<BTreeSet<_>>();
    // Existing order is significant. Keep it, then append newly selected IDs deterministically.
    let mut after = entry
        .overlays
        .iter()
        .filter(|path| requested.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    for path in requested {
        if !after.contains(&path) {
            after.push(path);
        }
    }
    Ok(Some(OverlayBootChange {
        kernel: entry.kernel.clone(),
        path: entry.path.clone(),
        devicetree_before: entry.devicetree.clone(),
        devicetree_after: entry.available_devicetree.clone(),
        overlays_before: entry.overlays.clone(),
        overlays_after: after,
    }))
}

struct Context {
    root: PathBuf,
    directory: PathBuf,
    entry: PathBuf,
    entry_bytes: Vec<u8>,
    base_source: PathBuf,
    base_target: PathBuf,
    base_bytes: Vec<u8>,
    files: BTreeMap<String, PathBuf>,
    status: OverlayStatus,
}

impl Context {
    fn load(root: &Path) -> Result<Self, HardwareError> {
        if crate::hardware::uefi_boot_mode(root) != Some("uefi-dt") {
            return Err(unsupported(
                "EFI Overlay management requires a Device Tree boot.",
            ));
        }
        let token = read_text(root, &root.join("etc/kernel/entry-token"))?
            .trim()
            .to_owned();
        let kernel = read_text(root, &root.join("proc/sys/kernel/osrelease"))?
            .trim()
            .to_owned();
        component(&token)?;
        component(&kernel)?;
        let esp = root.join("boot/efi");
        let kernel_dir = esp.join(&token).join(&kernel);
        ensure_path(root, &kernel_dir, false)?;
        let directory = kernel_dir.join("dtbo");
        ensure_path(root, &directory, false)?;
        let prefix = format!("{token}-{kernel}");
        let mut candidates = Vec::new();
        let mut revision_bytes = Vec::new();
        let mut other_references = Vec::new();
        let entries_dir = esp.join("loader/entries");
        ensure_path(root, &entries_dir, false)?;
        let mut entries = fs::read_dir(&entries_dir)
            .map_err(io_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_error)?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".conf") {
                continue;
            }
            let bytes = read_checked(root, &entry.path())?;
            revision_bytes.extend_from_slice(name.as_bytes());
            revision_bytes.extend_from_slice(&bytes);
            let text = std::str::from_utf8(&bytes).map_err(|_| invalid("non-UTF8 BLS entry"))?;
            if matches_entry_name(&name, &prefix) {
                candidates.push((entry.path(), bytes));
            } else {
                other_references.extend(values(text, "devicetree-overlay").into_iter().flat_map(
                    |value| {
                        value
                            .split_whitespace()
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    },
                ));
            }
        }
        if candidates.len() != 1 {
            return Err(unsupported(
                "A unique BLS entry for the running kernel was not found.",
            ));
        }
        let (entry, entry_bytes) = candidates.remove(0);
        let text = std::str::from_utf8(&entry_bytes).map_err(|_| invalid("non-UTF8 BLS entry"))?;
        if single_value(text, "version")?.is_some_and(|value| value != kernel) {
            return Err(invalid("BLS version does not match the running kernel"));
        }
        let linux = single_value(text, "linux")?
            .ok_or_else(|| unsupported("Only Linux BLS Type #1 entries are supported."))?;
        let linux = esp_path(root, &esp, linux)?;
        if linux.parent() != Some(kernel_dir.as_path()) || !linux.is_file() {
            return Err(invalid(
                "BLS kernel path does not match its version directory",
            ));
        }
        if !values(text, "efi").is_empty() || !values(text, "uki").is_empty() {
            return Err(unsupported(
                "EFI/UKI executable entries are not editable by the DT backend.",
            ));
        }
        let mut files = BTreeMap::new();
        let mut folded = BTreeSet::new();
        let mut listing = fs::read_dir(&directory)
            .map_err(io_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_error)?;
        listing.sort_by_key(|entry| entry.file_name());
        for file in listing {
            let name = file.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".disabled").unwrap_or(&name);
            if !id.ends_with(".dtbo") {
                continue;
            }
            component(id)?;
            if !folded.insert(id.to_ascii_lowercase()) {
                return Err(invalid("duplicate or case-colliding EFI Overlay ID"));
            }
            let bytes = read_checked(root, &file.path())?;
            validate_fdt(&bytes)?;
            revision_bytes.extend_from_slice(name.as_bytes());
            revision_bytes.extend_from_slice(&bytes);
            files.insert(id.to_owned(), file.path());
        }
        let overlays_text = single_value(text, "devicetree-overlay")?.unwrap_or("");
        let mut saved = Vec::new();
        for raw in overlays_text.split_whitespace() {
            let path = esp_path(root, &esp, raw)?;
            if path.parent() != Some(directory.as_path()) {
                return Err(unsupported(
                    "BLS references overlays outside the managed kernel directory.",
                ));
            }
            let id = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| invalid("invalid DTBO path"))?;
            if files.get(id) != Some(&path) {
                return Err(invalid("BLS references a missing or disabled Overlay file"));
            }
            let normalized = format!("/{}", raw.trim_start_matches('/'));
            if saved.contains(&normalized) {
                return Err(invalid("duplicate BLS Overlay reference"));
            }
            saved.push(normalized);
        }
        let directory_relative = format!("/{token}/{kernel}/dtbo/");
        if other_references.iter().any(|value| {
            format!("/{}", value.trim_start_matches('/'))
                .to_ascii_lowercase()
                .starts_with(&directory_relative.to_ascii_lowercase())
        }) {
            return Err(unsupported(
                "Overlay files are shared with another BLS entry; refusing to rename them.",
            ));
        }
        let before_dtb = single_value(text, "devicetree")?
            .filter(|s| !s.is_empty())
            .map(|s| format!("/{}", s.trim_start_matches('/')));
        if !saved.is_empty() && before_dtb.is_none() {
            return Err(invalid("BLS Overlays require a base device tree"));
        }
        let (base_source, base_target) = if let Some(path) = &before_dtb {
            let path = esp_path(root, &esp, path)?;
            (path.clone(), path)
        } else {
            let source = installed_base(root, &kernel)?;
            let target = kernel_dir.join(source.file_name().unwrap());
            ensure_path(root, &target, true)?;
            // Never replace an existing customized DTB while adding its first entry reference.
            (
                if target.exists() {
                    target.clone()
                } else {
                    source
                },
                target,
            )
        };
        let base_bytes = read_checked(root, &base_source)?;
        validate_fdt(&base_bytes)?;
        revision_bytes.extend_from_slice(display_path(root, &entry).as_bytes());
        revision_bytes.extend_from_slice(display_path(root, &base_source).as_bytes());
        revision_bytes.extend_from_slice(&base_bytes);
        revision_bytes.extend_from_slice(token.as_bytes());
        revision_bytes.extend_from_slice(kernel.as_bytes());
        let mut overlays = read_overlays(&directory)?;
        for overlay in &mut overlays {
            overlay.enabled = saved.contains(&format!("{directory_relative}{}", overlay.id));
        }
        let config = OverlayBootConfig {
            kernel,
            path: display_path(root, &entry),
            devicetree: before_dtb,
            available_devicetree: format!("/{}", base_target.strip_prefix(&esp).unwrap().display()),
            overlays: saved,
        };
        let status = OverlayStatus {
            collected_at: Utc::now(),
            synthetic: false,
            supported: true,
            mutable: !overlays.is_empty(),
            bootloader: "uefi-dt".into(),
            configuration_known: true,
            requires_authorization: false,
            cached: false,
            boot_entry: Some(config.clone()),
            directory: Some(display_path(root, &directory)),
            revision: fingerprint("efi-overlays-v1", &revision_bytes),
            overlays,
            unavailable_reason: None,
        };
        Ok(Self {
            root: root.into(),
            directory,
            entry,
            entry_bytes,
            base_source,
            base_target,
            base_bytes,
            files,
            status,
        })
    }

    fn status(&self) -> OverlayStatus {
        self.status.clone()
    }
}

fn installed_base(root: &Path, kernel: &str) -> Result<PathBuf, HardwareError> {
    let compatibles = read_checked(root, &root.join("proc/device-tree/compatible"))
        .or_else(|_| read_checked(root, &root.join("sys/firmware/devicetree/base/compatible")))?;
    let board = compatibles
        .split(|b| *b == 0)
        .filter_map(|s| std::str::from_utf8(s).ok())
        .find_map(|s| s.strip_prefix("radxa,"))
        .ok_or_else(|| unsupported("No exact Radxa DTB identity was found."))?;
    component(board)?;
    let suffix = format!("-{board}.dtb");
    let base = root.join(format!("usr/lib/linux-image-{kernel}"));
    ensure_path(root, &base, false)?;
    let mut found = Vec::new();
    // Kernel packages use a flat tree or one SoC-vendor directory; do not follow links.
    for entry in fs::read_dir(&base).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        ensure_path(root, &entry.path(), false)?;
        if entry.file_type().map_err(io_error)?.is_dir() {
            for child in fs::read_dir(entry.path()).map_err(io_error)? {
                let child = child.map_err(io_error)?;
                if child.file_name().to_string_lossy().ends_with(&suffix) {
                    found.push(child.path());
                }
            }
        } else if entry.file_name().to_string_lossy().ends_with(&suffix) {
            found.push(entry.path());
        }
    }
    if found.len() != 1 {
        return Err(unsupported(
            "A unique installed DTB for this SBC and kernel was not found.",
        ));
    }
    ensure_path(root, &found[0], false)?;
    Ok(found.remove(0))
}

fn values<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let end = line.find(char::is_whitespace).unwrap_or(line.len());
            (line[..end] == *key).then(|| line[end..].trim())
        })
        .collect()
}
fn single_value<'a>(text: &'a str, key: &str) -> Result<Option<&'a str>, HardwareError> {
    let values = values(text, key);
    if values.len() > 1 {
        return Err(invalid(&format!("duplicate BLS key: {key}")));
    }
    Ok(values.first().copied())
}
fn matches_entry_name(name: &str, prefix: &str) -> bool {
    if name == format!("{prefix}.conf") {
        return true;
    }
    let Some(counter) = name
        .strip_prefix(&format!("{prefix}+"))
        .and_then(|s| s.strip_suffix(".conf"))
    else {
        return false;
    };
    let parts = counter.split('-').collect::<Vec<_>>();
    parts.len() <= 2
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}
fn component(value: &str) -> Result<(), HardwareError> {
    if value.is_empty()
        || value.len() > 255
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
    {
        return Err(invalid("unsafe EFI path component"));
    }
    Ok(())
}
fn esp_path(root: &Path, esp: &Path, value: &str) -> Result<PathBuf, HardwareError> {
    let relative = value.strip_prefix('/').unwrap_or(value);
    for part in relative.split('/') {
        component(part)?;
    }
    let path = esp.join(relative);
    ensure_path(root, &path, false)?;
    Ok(path)
}
fn ensure_path(root: &Path, path: &Path, missing_leaf: bool) -> Result<(), HardwareError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| invalid("EFI path escapes system root"))?;
    let mut current = root.to_path_buf();
    for part in relative.components() {
        let Component::Normal(part) = part else {
            return Err(invalid("non-normal EFI path"));
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !(metadata.is_dir() || metadata.is_file()) {
                    return Err(invalid(
                        "EFI paths must be regular files or directories, not links",
                    ));
                }
                if root == Path::new("/")
                    && !current.starts_with("/proc")
                    && !current.starts_with("/sys")
                    && (metadata.uid() != 0 || metadata.mode() & 0o022 != 0)
                {
                    return Err(invalid(
                        "EFI input is not owned exclusively by administrators",
                    ));
                }
            }
            Err(error)
                if missing_leaf && current == path && error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(())
}
fn read_checked(root: &Path, path: &Path) -> Result<Vec<u8>, HardwareError> {
    ensure_path(root, path, false)?;
    let metadata = fs::metadata(path).map_err(io_error)?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(invalid("invalid or oversized EFI file"));
    }
    let bytes = fs::read(path).map_err(io_error)?;
    if bytes.len() as u64 > LIMIT {
        return Err(invalid("oversized EFI file"));
    }
    Ok(bytes)
}
fn read_text(root: &Path, path: &Path) -> Result<String, HardwareError> {
    String::from_utf8(read_checked(root, path)?).map_err(|_| invalid("non-UTF8 EFI configuration"))
}
fn validate_fdt(bytes: &[u8]) -> Result<(), HardwareError> {
    if bytes.len() < 40 || bytes[..4] != [0xd0, 0x0d, 0xfe, 0xed] {
        return Err(invalid("invalid DTB/DTBO header"));
    }
    let size = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if size < 40 || size > bytes.len() {
        return Err(invalid("truncated DTB/DTBO"));
    }
    Ok(())
}
fn io_error(error: io::Error) -> HardwareError {
    if error.kind() == io::ErrorKind::PermissionDenied {
        HardwareError::RootRequired
    } else {
        HardwareError::Io(error.to_string())
    }
}
fn invalid(message: &str) -> HardwareError {
    HardwareError::InvalidInput(message.into())
}
fn unsupported(message: &str) -> HardwareError {
    HardwareError::Unsupported(message.into())
}

struct ApplyLock(File);
impl ApplyLock {
    fn acquire(root: &Path) -> Result<Self, HardwareError> {
        let directory = root.join("run/rsetup-next-overlays");
        private_directory(root, &directory)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join("rsetup-next-overlays.lock"))
            .map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
            || (root == Path::new("/") && metadata.uid() != 0)
        {
            return Err(invalid("unsafe Overlay lock file"));
        }
        // SAFETY: the owned fd remains live for the complete transaction.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(HardwareError::Conflict(
                "Another Overlay operation is in progress.".into(),
            ));
        }
        Ok(Self(file))
    }
}
impl Drop for ApplyLock {
    fn drop(&mut self) {
        // SAFETY: the file is still owned here.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub(crate) fn apply(
    root: &Path,
    selected: &[String],
    token: &str,
    plan: impl FnOnce(OverlayStatus) -> Result<OverlayPlan, HardwareError>,
) -> Result<OverlayApplyResult, HardwareError> {
    let _lock = ApplyLock::acquire(root)?;
    let context = Context::load(root)?;
    let plan = plan(context.status())?;
    if plan.plan_token != token
        || plan.selected_ids
            != selected
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
    {
        return Err(HardwareError::StalePlan);
    }
    let started = Utc::now();
    let backup = if plan.changes.is_empty() {
        None
    } else {
        validate_combination(&context, &plan)?;
        Some(transact(&context, &plan, |_| Ok(()))?)
    };
    let status = Context::load(root)?.status();
    Ok(OverlayApplyResult {
        run: hardware_run(
            "hardware.overlays",
            "Switch device-tree overlays",
            ActionStatus::Succeeded,
            false,
            "EFI Overlay selection saved. Boot the selected kernel to activate it.",
            backup.map(|path| format!("Backup: {}", display_path(root, &path))),
            started,
        ),
        reboot_required: !plan.changes.is_empty(),
        plan,
        status: Some(status),
    })
}

fn validate_combination(context: &Context, plan: &OverlayPlan) -> Result<(), HardwareError> {
    if plan.selected_ids.is_empty() {
        return Ok(());
    }
    let temporary = std::env::temp_dir().join(format!("rsetup-dt-{}", Uuid::new_v4()));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&temporary)
        .map_err(io_error)?;
    let output = temporary.join("validated.dtb");
    let change = plan
        .boot_change
        .as_ref()
        .ok_or_else(|| invalid("missing EFI plan"))?;
    let mut command = Command::new("fdtoverlay");
    command
        .arg("-i")
        .arg(&context.base_source)
        .arg("-o")
        .arg(&output);
    for path in &change.overlays_after {
        let id = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid("invalid Overlay path"))?;
        command.arg(&context.files[id]);
    }
    let result = command.output();
    let _ = fs::remove_file(&output);
    let _ = fs::remove_dir(&temporary);
    match result {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(HardwareError::Conflict(format!(
            "DTB/Overlay validation failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(1000)
                .collect::<String>()
        ))),
        Err(error) => Err(unsupported(&format!(
            "fdtoverlay is required to validate EFI changes: {error}"
        ))),
    }
}

fn render_entry(before: &[u8], change: &OverlayBootChange) -> Result<Vec<u8>, HardwareError> {
    let text = std::str::from_utf8(before).map_err(|_| invalid("non-UTF8 BLS entry"))?;
    let mut after = String::new();
    for line in text.split_inclusive('\n') {
        let key = line.split_whitespace().next().unwrap_or("");
        if !matches!(key, "devicetree" | "devicetree-overlay") {
            after.push_str(line);
        }
    }
    if !after.is_empty() && !after.ends_with('\n') {
        after.push('\n');
    }
    after.push_str(&format!("devicetree {}\n", change.devicetree_after));
    if !change.overlays_after.is_empty() {
        after.push_str(&format!(
            "devicetree-overlay {}\n",
            change.overlays_after.join(" ")
        ));
    }
    Ok(after.into_bytes())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), HardwareError> {
    let parent = path.parent().ok_or_else(|| invalid("missing parent"))?;
    let temp = parent.join(format!(".rsetup-next-{}", Uuid::new_v4()));
    let result = (|| {
        let mode = fs::metadata(path)
            .map(|meta| meta.mode() & 0o777)
            .unwrap_or(0o600);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)
            .map_err(io_error)?;
        file.write_all(bytes).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        fs::rename(&temp, path).map_err(io_error)?;
        sync_directory(parent)
    })();
    let _ = fs::remove_file(&temp);
    result
}
fn sync_directory(path: &Path) -> Result<(), HardwareError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(io_error)
}

fn private_directory(root: &Path, path: &Path) -> Result<(), HardwareError> {
    let mut current = root.to_owned();
    for part in path
        .strip_prefix(root)
        .map_err(|_| invalid("invalid private directory"))?
        .components()
    {
        let Component::Normal(part) = part else {
            return Err(invalid("invalid private directory"));
        };
        current.push(part);
        ensure_path(root, &current, true)?;
        if !current.exists() {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&current)
                .map_err(io_error)?;
        }
        if !current.is_dir() {
            return Err(invalid("private directory is not a directory"));
        }
    }
    if fs::metadata(path).map_err(io_error)?.mode() & 0o077 != 0 {
        return Err(invalid("Overlay private directory must have mode 0700"));
    }
    Ok(())
}

fn transact(
    context: &Context,
    plan: &OverlayPlan,
    mut checkpoint: impl FnMut(&str) -> Result<(), HardwareError>,
) -> Result<PathBuf, HardwareError> {
    let change = plan
        .boot_change
        .as_ref()
        .ok_or_else(|| invalid("missing EFI plan"))?;
    let after = render_entry(&context.entry_bytes, change)?;
    let backup_parent = context.root.join("var/lib/rsetup-next/overlay-backups");
    private_directory(&context.root, &backup_parent)?;
    let backup = backup_parent.join(Uuid::new_v4().to_string());
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&backup)
        .map_err(io_error)?;
    atomic_write(&backup.join("entry.before"), &context.entry_bytes)?;
    atomic_write(&backup.join("entry.after"), &after)?;
    atomic_write(&backup.join("base.dtb"), &context.base_bytes)?;
    atomic_write(
        &backup.join("plan.json"),
        &serde_json::to_vec_pretty(plan).map_err(|e| HardwareError::Io(e.to_string()))?,
    )?;
    let mut moves = Vec::new();
    let mut original = Vec::new();
    for id in &plan.selected_ids {
        let from = &context.files[id];
        let to = context.directory.join(id);
        if *from != to {
            original.push((from.clone(), to));
        }
    }
    for (id, from) in &context.files {
        if !plan.selected_ids.contains(id)
            && context
                .status
                .overlays
                .iter()
                .any(|overlay| overlay.id == *id && overlay.enabled)
        {
            original.push((
                from.clone(),
                context.directory.join(format!("{id}.disabled")),
            ));
        }
    }
    for (index, (from, to)) in original.iter().enumerate() {
        ensure_path(&context.root, to, true)?;
        if to.exists() {
            return Err(invalid("EFI Overlay destination already exists"));
        }
        atomic_write(
            &backup.join(format!("overlay-{index}.dtbo")),
            &read_checked(&context.root, from)?,
        )?;
    }
    atomic_write(
        &backup.join("moves.json"),
        &serde_json::to_vec_pretty(
            &original
                .iter()
                .map(|(a, b)| {
                    (
                        display_path(&context.root, a),
                        display_path(&context.root, b),
                    )
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|e| HardwareError::Io(e.to_string()))?,
    )?;
    // Re-read all inputs after staging the backup, before the first boot-file mutation.
    if Context::load(&context.root)?.status.revision != plan.revision {
        return Err(HardwareError::StalePlan);
    }
    let mut entry_written = false;
    let mut base_created = false;
    let result: Result<(), HardwareError> = (|| {
        if !context.base_target.exists() {
            base_created = true;
            atomic_write(&context.base_target, &context.base_bytes)?;
        }
        // New references must exist before the atomic entry replacement.
        for (from, to) in &original {
            if to.extension().is_some_and(|s| s == "dtbo") {
                fs::rename(from, to).map_err(io_error)?;
                moves.push((from.clone(), to.clone()));
            }
        }
        sync_directory(&context.directory)?;
        checkpoint("enabled")?;
        // Mark before atomic_write: even a directory fsync failure may follow a successful rename.
        entry_written = true;
        atomic_write(&context.entry, &after)?;
        checkpoint("entry")?;
        for (from, to) in &original {
            if to.extension().is_some_and(|s| s == "disabled") {
                fs::rename(from, to).map_err(io_error)?;
                moves.push((from.clone(), to.clone()));
            }
        }
        sync_directory(&context.directory)?;
        checkpoint("disabled")?;
        if read_checked(&context.root, &context.entry)? != after {
            return Err(HardwareError::Io("EFI entry readback mismatch".into()));
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        // Restore old references first, then the entry, then remove new references.
        for (from, to) in moves
            .iter()
            .rev()
            .filter(|(_, to)| to.extension().is_some_and(|s| s == "disabled"))
        {
            if let Err(e) = fs::rename(to, from) {
                failures.push(e.to_string());
            }
        }
        if entry_written {
            if let Err(e) = atomic_write(&context.entry, &context.entry_bytes) {
                failures.push(e.to_string());
            }
        }
        if failures.is_empty() {
            for (from, to) in moves
                .iter()
                .rev()
                .filter(|(_, to)| to.extension().is_some_and(|s| s == "dtbo"))
            {
                if let Err(e) = fs::rename(to, from) {
                    failures.push(e.to_string());
                }
            }
            if base_created {
                if let Err(e) = fs::remove_file(&context.base_target) {
                    failures.push(e.to_string());
                }
            }
        }
        let _ = sync_directory(&context.directory);
        return Err(HardwareError::Io(format!(
            "{error}; backup: {}; rollback: {}",
            display_path(&context.root, &backup),
            if failures.is_empty() {
                "completed".into()
            } else {
                failures.join("; ")
            }
        )));
    }
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::HardwareManager;
    use std::os::unix::fs::{PermissionsExt, symlink};

    const KERNEL: &str = "7.0.11-5-qcom";
    const UART: &str = "sc8280xp-uart18.dtbo";
    const SPI: &str = "sc8280xp-spi4-spidev.dtbo";

    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new(board: &str) -> Self {
            let root = std::env::temp_dir().join(format!("rsetup-efi-test-{}", Uuid::new_v4()));
            let fixture = Self { root };
            for directory in [
                "sys/firmware/efi",
                "sys/firmware/devicetree/base",
                "proc/sys/kernel",
                "etc/kernel",
                "boot/efi/loader/entries",
                "boot/efi/RadxaOS/7.0.11-5-qcom/dtbo",
                "usr/lib/linux-image-7.0.11-5-qcom/qcom",
            ] {
                fs::create_dir_all(fixture.root.join(directory)).unwrap();
            }
            fixture.write("etc/kernel/entry-token", b"RadxaOS\n");
            fixture.write("proc/sys/kernel/osrelease", KERNEL.as_bytes());
            fixture.write(
                "sys/firmware/devicetree/base/compatible",
                format!("radxa,{board}\0qcom,sc8280xp\0").as_bytes(),
            );
            fixture.write("boot/efi/RadxaOS/7.0.11-5-qcom/linux", b"kernel");
            fixture.write(
                &format!("usr/lib/linux-image-{KERNEL}/qcom/sc8280xp-{board}.dtb"),
                &fdt(),
            );
            // The EL2 variant must not be picked accidentally.
            fixture.write(
                &format!("usr/lib/linux-image-{KERNEL}/qcom/sc8280xp-{board}-el2.dtb"),
                &fdt(),
            );
            fixture.write(
                &format!("boot/efi/RadxaOS/{KERNEL}/dtbo/{UART}.disabled"),
                &fdt(),
            );
            fixture.write(&format!("boot/efi/RadxaOS/{KERNEL}/dtbo/{SPI}"), &fdt());
            fixture.write(&fixture.entry_relative(), format!("# keep comment\ntitle RadxaOS\nversion {KERNEL}\noptions root=UUID=test quiet\nlinux /RadxaOS/{KERNEL}/linux\ninitrd /RadxaOS/{KERNEL}/initrd\ninitrd /extra-ucode\n").as_bytes());
            fixture.write(
                "boot/efi/loader/entries/RadxaOS-7.0.11-6-qcom.conf",
                b"title newer kernel\nlinux /RadxaOS/7.0.11-6-qcom/linux\n",
            );
            fixture.write(
                "boot/efi/loader/loader.conf",
                b"default RadxaOS-7.0.11-6-qcom.conf\n",
            );
            fixture
        }
        fn write(&self, path: &str, bytes: &[u8]) {
            fs::write(self.root.join(path), bytes).unwrap();
        }
        fn entry_relative(&self) -> String {
            format!("boot/efi/loader/entries/RadxaOS-{KERNEL}.conf")
        }
        fn entry(&self) -> PathBuf {
            self.root.join(self.entry_relative())
        }
        fn manager(&self) -> HardwareManager {
            HardwareManager::at_root(self.root.clone())
        }
        fn plan(&self, ids: &[&str]) -> OverlayPlan {
            self.manager()
                .plan_overlays(&ids.iter().map(|id| id.to_string()).collect::<Vec<_>>())
                .unwrap()
        }
        fn context(&self) -> Context {
            Context::load(&self.root).unwrap()
        }
        fn save(&self, ids: &[&str]) -> PathBuf {
            transact(&self.context(), &self.plan(ids), |_| Ok(())).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    // A real, minimal empty FDT: reservation terminator + BEGIN_NODE/END_NODE/END.
    fn fdt() -> Vec<u8> {
        let header: [u32; 10] = [0xd00dfeed, 72, 56, 72, 40, 17, 16, 0, 0, 16];
        let mut bytes = header
            .into_iter()
            .flat_map(u32::to_be_bytes)
            .collect::<Vec<_>>();
        bytes.extend([0u8; 16]);
        for token in [1u32, 0, 2, 9] {
            bytes.extend(token.to_be_bytes());
        }
        bytes
    }

    #[test]
    fn q6a_and_q8b_use_the_same_backend_and_exact_base_tree() {
        for board in ["dragon-q6a", "dragon-q8b"] {
            let fixture = Fixture::new(board);
            let status = fixture.manager().overlay_status().unwrap();
            assert!(status.mutable && status.configuration_known);
            assert_eq!(status.bootloader, "uefi-dt");
            let entry = status.boot_entry.unwrap();
            assert_eq!(entry.kernel, KERNEL);
            assert_eq!(
                entry.available_devicetree,
                format!("/RadxaOS/{KERNEL}/sc8280xp-{board}.dtb")
            );
            // An unreferenced .dtbo is NOT an enabled boot configuration.
            assert!(status.overlays.iter().all(|overlay| !overlay.enabled));
            assert!(entry.overlays.is_empty());
        }
    }

    #[test]
    fn transaction_only_changes_reviewed_files_and_preserves_other_boot_entries() {
        let fixture = Fixture::new("dragon-q8b");
        let original = fs::read(fixture.entry()).unwrap();
        let backup = fixture.save(&[UART]);
        let context = fixture.context();
        assert_eq!(fs::read(backup.join("entry.before")).unwrap(), original);
        assert_eq!(fs::metadata(&backup).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            context.status.boot_entry.as_ref().unwrap().overlays,
            [format!("/RadxaOS/{KERNEL}/dtbo/{UART}")]
        );
        assert!(context.files[UART].ends_with(UART));
        assert!(
            context.files[SPI].ends_with(SPI),
            "unreviewed/unreferenced file stays untouched"
        );
        let text = String::from_utf8(fs::read(fixture.entry()).unwrap()).unwrap();
        assert!(text.starts_with(std::str::from_utf8(&original).unwrap()));
        assert_eq!(values(&text, "initrd").len(), 2);
        assert_eq!(
            fs::read_to_string(fixture.root.join("boot/efi/loader/loader.conf")).unwrap(),
            "default RadxaOS-7.0.11-6-qcom.conf\n"
        );
        assert_eq!(
            fs::read(
                fixture
                    .root
                    .join("boot/efi/loader/entries/RadxaOS-7.0.11-6-qcom.conf")
            )
            .unwrap(),
            b"title newer kernel\nlinux /RadxaOS/7.0.11-6-qcom/linux\n"
        );
        fixture.save(&[]);
        assert!(
            fixture
                .context()
                .status
                .boot_entry
                .unwrap()
                .overlays
                .is_empty()
        );
        assert!(fixture.context().files[UART].ends_with(format!("{UART}.disabled")));
    }

    #[test]
    fn gpio_resolves_the_saved_efi_selection_and_defaults() {
        let fixture = Fixture::new("dragon-q8b");
        fixture.save(&[UART]);
        let gpio = fixture.manager().gpio_status().unwrap();
        assert!(gpio.configuration_known);
        assert_eq!(gpio.configuration_kernel.as_deref(), Some(KERNEL));
        assert_eq!(gpio.configured_overlays, [UART]);
        for (number, function) in [(16, "UART18_TX"), (37, "UART18_RX"), (13, "GPIO_66")] {
            assert_eq!(
                gpio.pins
                    .iter()
                    .find(|pin| pin.physical_pin == number)
                    .unwrap()
                    .current_function
                    .as_deref(),
                Some(function)
            );
        }
    }

    #[test]
    fn boot_count_names_and_overlay_order_are_preserved() {
        let fixture = Fixture::new("dragon-q6a");
        fixture.save(&[UART]);
        fixture.save(&[SPI, UART]);
        let before = fixture.context().status.boot_entry.unwrap().overlays;
        assert!(before[0].ends_with(UART));
        assert!(before[1].ends_with(SPI));
        let counted = fixture
            .entry()
            .with_file_name(format!("RadxaOS-{KERNEL}+3-1.conf"));
        fs::rename(fixture.entry(), &counted).unwrap();
        let plan = fixture.plan(&[SPI]);
        assert!(
            plan.boot_change
                .as_ref()
                .unwrap()
                .path
                .ends_with("+3-1.conf")
        );
        transact(&fixture.context(), &plan, |_| Ok(())).unwrap();
        assert!(counted.exists() && !fixture.entry().exists());
        fs::copy(&counted, fixture.entry()).unwrap();
        assert!(
            Context::load(&fixture.root).is_err(),
            "ambiguous BLS entries must not be guessed"
        );
    }

    #[test]
    fn stale_plans_bind_entry_dtb_dtbo_and_entry_names() {
        for changed in ["entry", "dtb", "dtbo", "name", "newer"] {
            let fixture = Fixture::new("dragon-q8b");
            let plan = fixture.plan(&[UART]);
            match changed {
                "entry" => OpenOptions::new()
                    .append(true)
                    .open(fixture.entry())
                    .unwrap()
                    .write_all(b"options changed\n")
                    .unwrap(),
                "dtb" => OpenOptions::new()
                    .append(true)
                    .open(&fixture.context().base_source)
                    .unwrap()
                    .write_all(b"changed")
                    .unwrap(),
                "dtbo" => OpenOptions::new()
                    .append(true)
                    .open(&fixture.context().files[UART])
                    .unwrap()
                    .write_all(b"changed")
                    .unwrap(),
                "name" => fs::rename(
                    fixture.entry(),
                    fixture
                        .entry()
                        .with_file_name(format!("RadxaOS-{KERNEL}+3.conf")),
                )
                .unwrap(),
                _ => fixture.write(
                    "boot/efi/loader/entries/RadxaOS-7.0.11-6-qcom.conf",
                    b"title changed\n",
                ),
            }
            let manager = fixture.manager();
            assert!(matches!(
                manager.apply_overlays_live(&plan.selected_ids, &plan.plan_token),
                Err(HardwareError::StalePlan)
            ));
            assert!(fixture.context().files[UART].ends_with(format!("{UART}.disabled")));
        }
    }

    #[test]
    fn each_interrupted_stage_restores_original_references_and_files() {
        for phase in ["enabled", "entry", "disabled"] {
            let fixture = Fixture::new("dragon-q8b");
            fixture.save(&[SPI]);
            let context = fixture.context();
            let original_revision = context.status.revision.clone();
            let plan = fixture.plan(&[UART]);
            let error = transact(&context, &plan, |stage| {
                if stage == phase {
                    Err(HardwareError::Io("injected failure".into()))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert!(error.to_string().contains("rollback: completed"));
            assert_eq!(fixture.context().status.revision, original_revision);
        }
    }

    #[test]
    fn initial_dtb_is_removed_when_a_transaction_rolls_back() {
        let fixture = Fixture::new("dragon-q8b");
        let context = fixture.context();
        assert!(!context.base_target.exists());
        assert!(
            transact(&context, &fixture.plan(&[UART]), |_| Err(invalid(
                "injected"
            )))
            .is_err()
        );
        assert!(!context.base_target.exists());
        assert_eq!(fixture.context().status.revision, context.status.revision);
    }

    #[test]
    fn unsafe_paths_missing_references_and_shared_assets_are_rejected() {
        for bad in [
            "traversal",
            "symlink",
            "case",
            "disabled",
            "kernel",
            "shared",
            "duplicate",
        ] {
            let fixture = Fixture::new("dragon-q8b");
            match bad {
                "traversal" => fixture.write("etc/kernel/entry-token", b"../RadxaOS"),
                "symlink" => {
                    let file = fixture.context().files[UART].clone();
                    fs::remove_file(&file).unwrap();
                    symlink(&fixture.context().base_source, &file).unwrap();
                }
                "case" => fixture.write(
                    &format!("boot/efi/RadxaOS/{KERNEL}/dtbo/SC8280XP-UART18.dtbo"),
                    &fdt(),
                ),
                "disabled" => OpenOptions::new()
                    .append(true)
                    .open(fixture.entry())
                    .unwrap()
                    .write_all(
                        format!("devicetree-overlay /RadxaOS/{KERNEL}/dtbo/{UART}\n").as_bytes(),
                    )
                    .unwrap(),
                "kernel" => fixture.write(
                    &fixture.entry_relative(),
                    b"linux /RadxaOS/7.0.11-6-qcom/linux\n",
                ),
                "shared" => fixture.write(
                    "boot/efi/loader/entries/other.conf",
                    format!("devicetree-overlay /RadxaOS/{KERNEL}/dtbo/{SPI}\n").as_bytes(),
                ),
                _ => OpenOptions::new()
                    .append(true)
                    .open(fixture.entry())
                    .unwrap()
                    .write_all(b"linux /another\n")
                    .unwrap(),
            }
            assert!(Context::load(&fixture.root).is_err(), "{bad}");
        }
    }

    #[test]
    fn locks_and_private_backups_reject_unsafe_files() {
        let fixture = Fixture::new("dragon-q8b");
        let first = ApplyLock::acquire(&fixture.root).unwrap();
        assert!(matches!(
            ApplyLock::acquire(&fixture.root),
            Err(HardwareError::Conflict(_))
        ));
        drop(first);
        let path = fixture
            .root
            .join("run/rsetup-next-overlays/rsetup-next-overlays.lock");
        fs::remove_file(&path).unwrap();
        symlink(fixture.entry(), path).unwrap();
        assert!(ApplyLock::acquire(&fixture.root).is_err());
        let parent = fixture.root.join("var/lib/rsetup-next/overlay-backups");
        fs::create_dir_all(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(transact(&fixture.context(), &fixture.plan(&[UART]), |_| Ok(())).is_err());
    }

    #[test]
    fn permission_errors_are_an_authorization_state_not_empty_configuration() {
        assert!(matches!(
            io_error(io::Error::from(io::ErrorKind::PermissionDenied)),
            HardwareError::RootRequired
        ));
    }

    #[test]
    #[ignore = "requires device-tree-compiler tools; run explicitly in packaging/HIL validation"]
    fn native_fdtoverlay_validation_and_apply() {
        let fixture = Fixture::new("dragon-q8b");
        let plan = fixture.plan(&[UART]);
        let result = fixture
            .manager()
            .apply_overlays_live(&plan.selected_ids, &plan.plan_token)
            .unwrap();
        assert!(result.reboot_required);
        assert!(
            result
                .status
                .unwrap()
                .overlays
                .iter()
                .any(|o| o.id == UART && o.enabled)
        );
        let context = fixture.context();
        fixture.write(
            &format!("boot/efi/RadxaOS/{KERNEL}/dtbo/{SPI}"),
            b"bad data",
        );
        assert!(
            validate_combination(
                &context,
                &fixture
                    .manager()
                    .plan_overlays_from_status(context.status(), &[SPI.into()])
                    .unwrap()
            )
            .is_err()
        );
    }
}
