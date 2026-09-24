//! Private discovery. Locks are never unlinked: all contenders lock one inode.
use super::*;
use crate::workbench::recording::fs::Directory;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;

pub enum Acquisition {
    Owner(InstanceLock),
    Existing(PathBuf),
}
pub struct InstanceLock {
    directory: Directory,
    lock: std::fs::File,
}
impl InstanceLock {
    pub fn acquire(paths: &WorkbenchPaths, config: &Path, data: &Path) -> Result<Acquisition> {
        let root = Directory::root(&paths.root)?;
        let runtime = root.dir("runtime", true)?;
        let scope = blake3::hash(&serde_json::to_vec(&(config, data))?)
            .to_hex()
            .to_string();
        let directory = runtime.dir(&scope, true)?;
        let lock = match directory.open("instance.lock", true) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                directory.open("instance.lock", false)?
            }
            Err(e) => return Err(e.into()),
        };
        let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc < 0 {
            let error = std::io::Error::last_os_error();
            ensure!(
                error.kind() == std::io::ErrorKind::WouldBlock,
                "instance lock unavailable"
            );
            return Ok(Acquisition::Existing(directory.path.join("instance.json")));
        }
        let owned = Self { directory, lock };
        owned.verify()?;
        // Only the lock owner can discard stale discovery, after safe-file checks.
        match owned.directory.open("instance.json", false) {
            Ok(file) => {
                let a = file.metadata()?;
                let b = owned.directory.entry_info("instance.json")?;
                ensure!(
                    (a.dev(), a.ino()) == (b.identity.device, b.identity.inode),
                    "instance entry changed"
                );
                owned.directory.remove("instance.json")?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(Acquisition::Owner(owned))
    }
    pub fn path(&self) -> PathBuf {
        self.directory.path.join("instance.json")
    }
    pub fn verify(&self) -> Result<()> {
        self.directory.verify_location()?;
        let pinned = self.lock.metadata()?;
        let current = self.directory.open("instance.lock", false)?.metadata()?;
        ensure!(
            (pinned.dev(), pinned.ino()) == (current.dev(), current.ino()),
            "instance lock changed"
        );
        Ok(())
    }
}
