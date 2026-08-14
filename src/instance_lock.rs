use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(not(unix))]
use anyhow::bail;
use anyhow::{Context, Result};

/// Advisory process lock guarding every writer that targets one Observer database.
pub struct InstanceLock {
    file: File,
    path: PathBuf,
}

impl InstanceLock {
    pub fn acquire(database_path: &Path) -> Result<Self> {
        let directory = database_path
            .parent()
            .context("database path must have a parent directory")?;
        fs::create_dir_all(directory)?;
        let path = directory.join("observer.lock");
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("open Observer lock {}", path.display()))?;
        lock_exclusive_nonblocking(&file)
            .with_context(|| format!("another Observer writer already owns {}", path.display()))?;
        Ok(Self { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        if let Err(error) = unlock(&self.file) {
            tracing::warn!(path = %self.path.display(), error = %error, "failed to release Observer lock");
        }
    }
}

#[cfg(unix)]
fn lock_exclusive_nonblocking(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: flock only reads the valid file descriptor owned by `file`.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn unlock(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: flock only reads the valid file descriptor owned by `file`.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn lock_exclusive_nonblocking(_file: &File) -> io::Result<()> {
    bail!("single-instance locking is currently supported on Unix only")
}

#[cfg(not(unix))]
fn unlock(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn lock_is_exclusive_and_released_on_drop() -> Result<()> {
        let temp = TempDir::new()?;
        let database = temp.path().join("observer.sqlite");
        let first = InstanceLock::acquire(&database)?;
        assert!(InstanceLock::acquire(&database).is_err());
        assert_eq!(first.path(), temp.path().join("observer.lock"));
        drop(first);
        let _second = InstanceLock::acquire(&database)?;
        Ok(())
    }
}
