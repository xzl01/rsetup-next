//! Opening an image for the write loop: the raw or `.xz` opener that re-checks
//! the plan's file identity on the handle it actually reads, the spawning of
//! the `xz -dc` subprocess, and the output stream that only reports end of
//! output once the tool has exited successfully.

use std::fs::File;
use std::io::Read;
use std::process::{Child, ChildStdout, Command, Stdio};

use crate::model::{InstallImageFormat, SystemInstallImage};

use super::ImageOpener;
use super::error::SystemInstallError;
use super::image::file_identity;

/// Opens raw images directly and `.xz` images through `xz -dc`, the same
/// "call the required system tool" boundary the other managers use.
pub struct SystemImageOpener;

impl ImageOpener for SystemImageOpener {
    /// Opens the image and refuses it when the file is no longer the one the plan
    /// described.
    ///
    /// The plan bound the file's identity, but binding it is only useful if the
    /// bytes that reach the device come from the same file: a replacement between
    /// the check and the write would otherwise be written and reported as a
    /// success. The identity is therefore checked on the handle that is actually
    /// read, and a compressed image is decompressed from that handle rather than
    /// from the path a second time.
    fn open(&self, image: &SystemInstallImage) -> Result<Box<dyn Read + Send>, SystemInstallError> {
        let file = File::open(&image.path).map_err(|error| {
            SystemInstallError::Io(format!("unable to open {}: {error}", image.path))
        })?;
        let observed = file.metadata().map_err(|error| {
            SystemInstallError::Io(format!("unable to stat {}: {error}", image.path))
        })?;
        if file_identity(&observed) != image.file_identity {
            return Err(SystemInstallError::StalePlan);
        }
        match image.format {
            InstallImageFormat::Raw => Ok(Box::new(file)),
            InstallImageFormat::Xz => {
                let mut command = Command::new("xz");
                command
                    .arg("-dc")
                    // Decompress the verified handle, not the path.
                    .stdin(Stdio::from(file))
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
pub(super) fn spawn_decompressor(command: &mut Command) -> std::io::Result<Decompressor> {
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
pub(super) struct Decompressor {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SystemInstallRequest;
    use crate::system_install::test_support::*;
    use crate::system_install::{
        IMAGE_PREFIX_BYTES, MIB, SystemBlockBackend, SystemInstallError, SystemInstallManager,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn a_corrupt_stream_that_produced_all_of_its_output_is_refused() {
        if !xz_available() {
            return;
        }
        let harness = fixture("xz-corrupt-full", boot_mounts(), RereadBehavior::Matches);
        // Offset 30 sits in the compressed data, so the frames stay valid and the
        // plan is accepted: only the decompressor's exit status reveals the damage.
        for (name, bytes, offset, expected) in [
            ("single.img.xz", XZ_FIXTURE.to_vec(), 30, XZ_FIXTURE_BYTES),
            (
                "two.img.xz",
                [XZ_FIXTURE, XZ_FIXTURE].concat(),
                XZ_FIXTURE.len() + 30,
                XZ_FIXTURE_BYTES * 2,
            ),
        ] {
            let path = harness.root.join(name);
            write_file(&path, flip(&bytes, offset));
            // Damage inside the prefix the planner decompresses is now caught
            // while planning; damage past it still surfaces while writing. Either
            // way the file is never reported as a successful install.
            let plan = match harness.manager.plan(&install_request(&path)) {
                Ok(plan) => plan,
                Err(SystemInstallError::InvalidInput(message)) => {
                    assert!(
                        message.contains("could not be decompressed"),
                        "{name}: {message}"
                    );
                    continue;
                }
                Err(other) => panic!("{name} must be refused with a reason: {other}"),
            };
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
            // Corrupting the block the planner has to decompress is refused while
            // planning; a stream that survives the prefix read is still caught
            // while writing. Both are hard failures.
            let plan = match harness.manager.plan(&install_request(&path)) {
                Ok(plan) => plan,
                Err(SystemInstallError::InvalidInput(_)) => continue,
                Err(other) => panic!("{name} must be refused with a reason: {other}"),
            };
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

    /// The F-T13-002 / F-T20-001 property, restated for the current design: a
    /// token is only usable by the process that signed and issued it. A second
    /// manager stands in for a second process (the privileged helper), and the
    /// token it carries — even a well formed one — buys it nothing.
    /// The reproduction from the finding: the plan bound the file's identity, but
    /// the write re-opened the path without checking the handle it got, so a
    /// replacement between the check and the write was written and reported as a
    /// success. The opener below swaps the file at exactly that moment.
    #[test]
    fn a_replacement_between_the_check_and_the_write_is_refused() {
        struct SwappingOpener {
            path: PathBuf,
            replacement: Vec<u8>,
        }
        impl ImageOpener for SwappingOpener {
            fn open(
                &self,
                image: &SystemInstallImage,
            ) -> Result<Box<dyn Read + Send>, SystemInstallError> {
                // The window the finding is about: the plan has been checked and
                // the write is about to open the image.
                write_file(&self.path, &self.replacement);
                SystemImageOpener.open(image)
            }
        }

        let root = scratch("swap-after-check");
        fs::create_dir_all(root.join("sys/class/block/sda")).expect("create class directory");
        write_attr(&root.join("sys/class/block/sda/size"), "131072\n");
        write_attr(&root.join("sys/class/block/sda/dev"), "8:0\n");
        write_file(&root.join("dev/sda"), vec![0u8; 4 * MIB as usize]);
        // Nothing is mounted, so the only gate in play is the image's identity.
        write_file(&root.join("proc/mounts"), b"");

        let image = root.join("system.img");
        write_file(&image, mbr_image(1, 2 * MIB));
        // Same length and a change past the hashed prefix, so nothing but the
        // file's identity can tell the replacement from the original.
        let mut replacement = mbr_image(1, 2 * MIB);
        let offset = (IMAGE_PREFIX_BYTES + IMAGE_PREFIX_BYTES / 2) as usize;
        replacement[offset] ^= 0xFF;
        assert_eq!(replacement.len(), mbr_image(1, 2 * MIB).len());
        let request = SystemInstallRequest {
            target_id: "sda".into(),
            image_path: Some(image.display().to_string()),
            image_id: None,
        };

        // Control: the same setup with the real opener writes normally, so the
        // refusal below is about the swap and not about the fixture.
        let plain = SystemInstallManager::with_backend(
            Arc::new(SystemBlockBackend::new(Some(&root))),
            Arc::new(SystemImageOpener),
            false,
        );
        let plan = plain.plan(&request).expect("plan");
        assert!(plain.apply_live(&request, &plan.plan_token, "sda").is_ok());

        // The swap happens after the plan was accepted and before the write.
        let swapping = SystemInstallManager::with_backend(
            Arc::new(SystemBlockBackend::new(Some(&root))),
            Arc::new(SwappingOpener {
                path: image.clone(),
                replacement,
            }),
            false,
        );
        let plan = swapping.plan(&request).expect("plan");
        match swapping.apply_live(&request, &plan.plan_token, "sda") {
            Err(SystemInstallError::StalePlan) => {}
            other => panic!("a replaced image must not be written: {other:?}"),
        }
    }
}
