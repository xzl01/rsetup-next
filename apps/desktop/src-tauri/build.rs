//! Publishes `ui/` into `apps/desktop/dist` for Tauri's asset embedding.
//!
//! `dist` is a fixed path (`tauri.conf.json` -> `build.frontendDist`) that every
//! desktop-crate build shares, no matter which `CARGO_TARGET_DIR` it uses, while
//! this build script runs once per cargo invocation. Rebuilding that tree with
//! `remove_dir_all` therefore deleted assets another concurrent build was still
//! embedding, which surfaced as
//!
//! ```text
//! error: failed to read asset at .../dist/fonts/open-sans-regular.woff2
//! because No such file or directory (os error 2)
//! ```
//!
//! or as a failed build script (the removal or the copy observing the peer's
//! directory operations).
//!
//! The published tree is never deleted now. Assets are staged outside `dist` and
//! moved into place with renames, which are atomic on one filesystem: a
//! concurrent reader observes either the previous or the new bytes of every
//! file, never a missing or half-written one. A whole missing tree is published
//! with a single directory rename. Staging lives outside `dist` on purpose,
//! because `tauri-codegen` embeds every file it walks with no hidden-file
//! filter, so a staging entry inside the tree would become an asset.
//!
//! ## Staging location and interrupted builds
//!
//! Each build stages into `<ignored generated directory>/rsetup-dist-staging/
//! <pid>`. The root is the crate's own `target/` directory (or the repository
//! `target/`), both already covered by `.gitignore` and on the same filesystem
//! as `dist`, which is what keeps publishing a plain rename. Beside `dist` there
//! is a last-resort root that `.gitignore` also covers for that exact name.
//!
//! A build removes exactly its own staging directory once publishing is done.
//! An interrupted build (SIGKILL, Ctrl-C) can leave one behind, so the next
//! build reclaims leftovers, and that is the only place this script deletes
//! anything besides its own directory:
//!
//! - a staging directory is reclaimed only when it has been untouched for
//!   [`STALE_AFTER`] **and** its owning pid no longer exists;
//! - directories that are fresh or owned by a live process are left alone,
//!   which is what makes reclamation safe while other builds are running;
//! - leftovers from the retired per-pid layout beside `dist` are reclaimed by
//!   the same rule, one time, so they do not linger after this change.
//!
//! The guarantee therefore holds for: builds younger than [`STALE_AFTER`] (a
//! live staging directory is refreshed by every entry it creates or removes,
//! and a whole desktop build script runs for milliseconds), and for any build
//! whose pid still exists when reclamation probes it. It does not cover a pid
//! that the OS recycles into a new build script which reaches its own staging
//! directory inside the microseconds between that probe and the removal, nor a
//! build suspended for longer than [`STALE_AFTER`] in the middle of staging
//! (its directory would look untouched); both would only cost a failed build
//! script, and both are accepted to keep interrupted builds from accumulating.
//!
//! Trade-off: because `dist` is never deleted, an asset removed from `ui/`
//! stays in `dist` until the tree is discarded by hand. That is reported as a
//! `cargo:warning` rather than left silent, and the previous behaviour stays
//! available by removing the generated tree (`rm -rf apps/desktop/dist`) while
//! no desktop build is running.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

/// Files copied verbatim into the root of the published frontend.
const ROOT_FILES: [&str; 4] = ["index.html", "app.js", "i18n.js", "styles.css"];

/// Directories copied recursively into the published frontend.
const TREE_DIRS: [&str; 2] = ["assets", "fonts"];

/// Container holding one staging directory per build process.
const STAGING_CONTAINER: &str = "rsetup-dist-staging";

/// Prefix of the retired layout that staged directly beside `dist`.
const LEGACY_STAGING_PREFIX: &str = ".rsetup-dist-staging-";

/// How long a staging directory must have been untouched before an interrupted
/// build's leftover is reclaimed. Every entry created or removed inside a
/// staging directory refreshes its timestamp, so a live one stays fresh.
const STALE_AFTER: Duration = Duration::from_secs(60);

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).expect("create staged asset directory");
    for entry in fs::read_dir(source).expect("read frontend assets") {
        let entry = entry.expect("frontend entry");
        let kind = entry.file_type().expect("asset type");
        let output = target.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &output);
        } else if kind.is_file() {
            fs::copy(entry.path(), &output).expect("stage frontend asset");
        }
    }
}

/// Copies the shared frontend into a private staging tree.
fn stage(ui: &Path, staging: &Path) {
    fs::create_dir_all(staging).expect("create staging directory");
    for name in ROOT_FILES {
        fs::copy(ui.join(name), staging.join(name)).expect("stage production frontend");
    }
    for name in TREE_DIRS {
        copy_tree(&ui.join(name), &staging.join(name));
    }
}

/// Lists the files under `root` as paths relative to it.
fn relative_files(root: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, directory: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                walk(root, &path, found);
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            if let Ok(relative) = path.strip_prefix(root) {
                found.push(relative.to_path_buf());
            }
        }
    }

    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort();
    found
}

/// Moves the staged tree into `dist`, replacing files in place.
fn publish(staging: &Path, output: &Path, staged: &[PathBuf]) {
    // A missing tree is published with one atomic directory rename; if another
    // build won that race, fall back to moving files into the tree it created.
    if !output.exists() && fs::rename(staging, output).is_ok() {
        return;
    }
    for relative in staged {
        let target = output.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).expect("create generated desktop frontend");
        }
        fs::rename(staging.join(relative), &target).expect("publish frontend asset");
    }
}

/// Reports assets that `dist` keeps but `ui/` no longer provides.
fn report_stale(output: &Path, staged: &[PathBuf]) {
    let expected = staged.iter().cloned().collect::<BTreeSet<_>>();
    let stale = relative_files(output)
        .into_iter()
        .filter(|relative| !expected.contains(relative))
        .collect::<Vec<_>>();
    if stale.is_empty() {
        return;
    }
    let names = stale
        .iter()
        .take(5)
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "cargo:warning=apps/desktop/dist still contains {} asset(s) that ui/ no longer provides \
         ({}). Concurrent builds are kept safe by never deleting the tree, so remove it with \
         `rm -rf apps/desktop/dist` while no desktop build is running if you need it pruned.",
        stale.len(),
        names
    );
}

/// Narrows `path` to its closest existing ancestor, for filesystem probes on a
/// directory that is about to be created.
fn existing_ancestor(path: &Path) -> PathBuf {
    let mut current = path;
    loop {
        if current.exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return current.to_path_buf(),
        }
    }
}

/// Whether two paths resolve onto one filesystem, which is what lets publishing
/// stay an atomic rename.
#[cfg(unix)]
fn same_filesystem(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(left) = fs::metadata(existing_ancestor(left)) else {
        return false;
    };
    let Ok(right) = fs::metadata(existing_ancestor(right)) else {
        return false;
    };
    left.dev() == right.dev()
}

#[cfg(not(unix))]
fn same_filesystem(_left: &Path, _right: &Path) -> bool {
    true
}

/// Picks an already ignored generated directory on the same filesystem as
/// `dist`: the crate's `target/`, then the repository `target/`, then - as a
/// last resort that `.gitignore` also names - the directory holding `dist`.
fn staging_root(manifest: &Path, output: &Path) -> PathBuf {
    let generated = output.parent().expect("generated frontend has a parent");
    [manifest.join("target"), manifest.join("../../../target")]
        .into_iter()
        .find(|candidate| same_filesystem(candidate, generated))
        .unwrap_or_else(|| generated.to_path_buf())
}

fn is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age >= STALE_AFTER)
}

/// Whether a process with that pid still exists. A probe that cannot run at all
/// reports alive, so reclamation never happens on a guess.
fn process_is_alive(pid: u32) -> bool {
    match Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
    {
        Ok(output) => !String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        Err(_) => true,
    }
}

fn directory_owned_by_dead_process(path: &Path, pid: u32) -> bool {
    let Ok(kind) = fs::symlink_metadata(path).map(|metadata| metadata.file_type()) else {
        return false;
    };
    kind.is_dir() && !kind.is_symlink() && is_stale(path) && !process_is_alive(pid)
}

/// Reclaims staging directories of interrupted builds, never a live one.
fn reclaim_stale_staging(container: &Path) {
    let Ok(entries) = fs::read_dir(container) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        if directory_owned_by_dead_process(&entry.path(), pid) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Reclaims leftovers of the retired per-pid staging layout, once.
fn reclaim_legacy_staging(desktop: &Path) {
    let Ok(entries) = fs::read_dir(desktop) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_prefix(LEGACY_STAGING_PREFIX)
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if directory_owned_by_dead_process(&entry.path(), pid) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let ui = manifest.join("../../../ui");
    let output = manifest.join("../dist");
    let container = staging_root(&manifest, &output).join(STAGING_CONTAINER);
    let staging = container.join(std::process::id().to_string());
    println!("cargo:rerun-if-changed={}", ui.display());

    fs::create_dir_all(&container).expect("create staging container");
    reclaim_stale_staging(&container);
    reclaim_legacy_staging(&manifest.join(".."));
    // A recycled pid can only match a leftover of a process that is gone.
    if staging.exists() {
        fs::remove_dir_all(&staging).expect("clear leftover staging directory");
    }

    stage(&ui, &staging);
    let staged = relative_files(&staging);
    publish(&staging, &output, &staged);
    if staging.exists() {
        fs::remove_dir_all(&staging).expect("clear staging directory");
    }
    report_stale(&output, &staged);
    tauri_build::build()
}
