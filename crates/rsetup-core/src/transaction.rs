//! Durable configuration replacement and advisory transaction locks.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::Path,
};

pub(crate) struct ProcessLock(File);
impl ProcessLock {
    pub(crate) fn acquire(root: &Path, name: &str) -> io::Result<Self> {
        let directory = root.join("run/lock");
        fs::create_dir_all(&directory)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.join(name))?;
        let metadata = file.metadata()?;
        // SAFETY: geteuid has no preconditions.
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(io::Error::other("unsafe transaction lock"));
        }
        // SAFETY: file owns the descriptor for the full lock lifetime.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::other(format!(
                "another configuration operation is running: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(Self(file))
    }
}
impl Drop for ProcessLock {
    fn drop(&mut self) {
        // SAFETY: File still owns this descriptor.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

pub(crate) fn atomic_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_replace_with_mode(path, bytes, None)
}

pub(crate) fn atomic_replace_with_mode(
    path: &Path,
    bytes: &[u8],
    mode: Option<u32>,
) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) if value.is_file() => Some(value),
        Ok(_) => return Err(io::Error::other("refusing to replace a non-regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("configuration has no parent"))?;
    let temporary = parent.join(format!(".rsetup-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        if let Some(metadata) = metadata {
            // SAFETY: fchown receives the descriptor owned by file.
            if unsafe { libc::geteuid() } == 0
                && unsafe { libc::fchown(file.as_raw_fd(), metadata.uid(), metadata.gid()) } != 0
            {
                return Err(io::Error::last_os_error());
            }
            file.set_permissions(metadata.permissions())?;
        } else {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o644))?;
        }
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_and_lock_fail_closed() {
        let root =
            std::env::temp_dir().join(format!("rsetup-transaction-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config");
        atomic_replace(&path, b"old").unwrap();
        atomic_replace(&path, b"new").unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(atomic_replace(&link, b"bad").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let lock = ProcessLock::acquire(&root, "test.lock").unwrap();
        assert!(ProcessLock::acquire(&root, "test.lock").is_err());
        drop(lock);
        assert!(ProcessLock::acquire(&root, "test.lock").is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
