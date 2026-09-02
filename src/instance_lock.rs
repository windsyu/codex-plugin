use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(not(unix))]
use anyhow::bail;
use anyhow::{Context, Result};

use crate::permissions::{prepare_private_dir, prepare_private_file};

/// Advisory process lock guarding every writer that targets one Observer database.
pub struct InstanceLock {
    file: File,
    path: PathBuf,
}

const PAIRING_READY: &[u8] = b"pairing-ready\n";

impl InstanceLock {
    pub fn acquire(database_path: &Path) -> Result<Self> {
        let directory = database_path
            .parent()
            .context("database path must have a parent directory")?;
        prepare_private_dir(directory, "Observer data")?;
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
        prepare_private_file(&path, "Observer lock")?;
        lock_exclusive_nonblocking(&file)
            .with_context(|| format!("another Observer writer already owns {}", path.display()))?;
        let mut lock = Self { file, path };
        lock.write_state(b"starting\n")?;
        Ok(lock)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn held_by_writer(database_path: &Path) -> Result<bool> {
        let directory = database_path
            .parent()
            .context("database path must have a parent directory")?;
        let path = directory.join("observer.lock");
        match path.symlink_metadata() {
            Ok(_) => prepare_private_file(&path, "Observer lock")?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect Observer lock {}", path.display()));
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open Observer lock {}", path.display()))?;
        match lock_exclusive_nonblocking(&file) {
            Ok(()) => {
                unlock(&file)
                    .with_context(|| format!("release Observer lock probe {}", path.display()))?;
                Ok(false)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(true),
            Err(error) => Err(error)
                .with_context(|| format!("probe Observer lock ownership {}", path.display())),
        }
    }

    pub fn pairing_ready(database_path: &Path) -> Result<bool> {
        if !Self::held_by_writer(database_path)? {
            return Ok(false);
        }
        let directory = database_path
            .parent()
            .context("database path must have a parent directory")?;
        let path = directory.join("observer.lock");
        Ok(std::fs::read(&path)
            .with_context(|| format!("read Observer lock state {}", path.display()))?
            == PAIRING_READY)
    }

    pub fn mark_pairing_ready(&mut self) -> Result<()> {
        self.write_state(PAIRING_READY)
    }

    fn write_state(&mut self, state: &[u8]) -> Result<()> {
        self.file
            .set_len(0)
            .with_context(|| format!("clear Observer lock state {}", self.path.display()))?;
        self.file
            .seek(SeekFrom::Start(0))
            .with_context(|| format!("seek Observer lock state {}", self.path.display()))?;
        self.file
            .write_all(state)
            .with_context(|| format!("write Observer lock state {}", self.path.display()))?;
        self.file
            .sync_all()
            .with_context(|| format!("sync Observer lock state {}", self.path.display()))?;
        Ok(())
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
        assert!(!InstanceLock::held_by_writer(&database)?);
        let mut first = InstanceLock::acquire(&database)?;
        assert!(InstanceLock::acquire(&database).is_err());
        assert!(InstanceLock::held_by_writer(&database)?);
        assert!(!InstanceLock::pairing_ready(&database)?);
        first.mark_pairing_ready()?;
        assert!(InstanceLock::pairing_ready(&database)?);
        assert_eq!(first.path(), temp.path().join("observer.lock"));
        drop(first);
        assert!(!InstanceLock::held_by_writer(&database)?);
        assert!(!InstanceLock::pairing_ready(&database)?);
        let _second = InstanceLock::acquire(&database)?;
        Ok(())
    }
}
