//! Private, descriptor-relative storage. Never follow a journal/blob symlink.
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(super) const FILE_LIMIT: usize = 128 * 1024 * 1024;

pub(crate) struct Directory {
    file: File,
    pub path: PathBuf,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    pub device: u64,
    pub inode: u64,
}
pub(crate) struct Entries(*mut libc::DIR);
impl Iterator for Entries {
    type Item = io::Result<String>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // readdir is confined to the owning worker and this descriptor.
            unsafe {
                *errno() = 0;
            }
            let entry = unsafe { libc::readdir(self.0) };
            if entry.is_null() {
                let code = unsafe { *errno() };
                return (code != 0).then(|| Err(io::Error::from_raw_os_error(code)));
            }
            let value = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if value.to_bytes() == b"." || value.to_bytes() == b".." {
                continue;
            }
            return Some(
                value
                    .to_str()
                    .map(str::to_owned)
                    .map_err(|_| io::ErrorKind::InvalidData.into()),
            );
        }
    }
}
#[cfg(target_os = "macos")]
unsafe fn errno() -> *mut libc::c_int {
    unsafe { libc::__error() }
}
#[cfg(target_os = "linux")]
unsafe fn errno() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}
impl Drop for Entries {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.0);
        }
    }
}
pub(crate) struct EntryInfo {
    pub identity: Identity,
    pub directory: bool,
    pub bytes: u64,
    pub modified: (i64, i64),
}
fn name(value: &str) -> io::Result<CString> {
    if value.is_empty() || value == "." || value == ".." || value.contains('/') {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    CString::new(value).map_err(|_| io::ErrorKind::InvalidInput.into())
}
fn owned(file: &File, directory: bool) -> io::Result<()> {
    let m = file.metadata()?;
    if m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || if directory {
            !m.is_dir()
        } else {
            !m.is_file() || m.nlink() != 1
        }
    {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    Ok(())
}
impl Directory {
    pub fn identity(&self) -> io::Result<Identity> {
        let m = self.file.metadata()?;
        Ok(Identity {
            device: m.dev(),
            inode: m.ino(),
        })
    }
    pub fn entries(&self) -> io::Result<Entries> {
        // Opening "." creates an independent directory offset (dup would share it).
        let dot = c".";
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                dot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let pointer = unsafe { libc::fdopendir(fd) };
        if pointer.is_null() {
            let error = io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(error);
        }
        Ok(Entries(pointer))
    }
    pub fn entry_info(&self, entry: &str) -> io::Result<EntryInfo> {
        let c = name(entry)?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                c.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let kind = stat.st_mode & libc::S_IFMT;
        if stat.st_uid != unsafe { libc::geteuid() }
            || stat.st_mode & 0o077 != 0
            || !matches!(kind, libc::S_IFDIR | libc::S_IFREG)
            || (kind == libc::S_IFREG && stat.st_nlink != 1)
            || stat.st_size < 0
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(EntryInfo {
            identity: Identity {
                device: stat.st_dev as u64,
                inode: stat.st_ino,
            },
            directory: kind == libc::S_IFDIR,
            bytes: stat.st_size as u64,
            modified: (stat.st_mtime, stat.st_mtime_nsec),
        })
    }
    pub fn remove_dir(&self, entry: &str, identity: Identity) -> io::Result<()> {
        self.verify_location()?;
        let info = self.entry_info(entry)?;
        if !info.directory || info.identity != identity {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let c = name(entry)?;
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), c.as_ptr(), libc::AT_REMOVEDIR) } < 0 {
            return Err(io::Error::last_os_error());
        }
        self.sync()
    }
    /// The target must not exist; both parents and the source inode are pinned.
    pub fn move_directory(
        &self,
        entry: &str,
        destination: &Directory,
        identity: Identity,
    ) -> io::Result<()> {
        self.verify_location()?;
        destination.verify_location()?;
        let info = self.entry_info(entry)?;
        if !info.directory
            || info.identity != identity
            || destination.identity()?.device != identity.device
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let c = name(entry)?;
        #[cfg(target_os = "macos")]
        let rc = unsafe {
            libc::renameatx_np(
                self.file.as_raw_fd(),
                c.as_ptr(),
                destination.file.as_raw_fd(),
                c.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        let rc = unsafe {
            libc::renameat2(
                self.file.as_raw_fd(),
                c.as_ptr(),
                destination.file.as_raw_fd(),
                c.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        self.sync()?;
        destination.sync()
    }
    /// A pinned directory must still be the directory named by the launcher.
    /// Do not write into an unlinked/replaced configuration root.
    pub fn verify_location(&self) -> io::Result<()> {
        let current = std::fs::symlink_metadata(&self.path)?;
        let pinned = self.file.metadata()?;
        if !current.is_dir() || current.dev() != pinned.dev() || current.ino() != pinned.ino() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        owned(&self.file, true)
    }
    pub fn root(path: &Path) -> io::Result<Self> {
        match std::fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        owned(&file, true)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }
    pub fn dir(&self, entry: &str, create: bool) -> io::Result<Self> {
        let c = name(entry)?;
        if create && unsafe { libc::mkdirat(self.file.as_raw_fd(), c.as_ptr(), 0o700) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        owned(&file, true)?;
        if create {
            self.sync()?;
        }
        Ok(Self {
            file,
            path: self.path.join(entry),
        })
    }
    pub fn open(&self, entry: &str, create: bool) -> io::Result<File> {
        let c = name(entry)?;
        let flags = if create {
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL
        } else {
            libc::O_RDONLY
        };
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                c.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        owned(&file, false)?;
        Ok(file)
    }
    pub fn read(&self, entry: &str, limit: usize) -> io::Result<Vec<u8>> {
        let file = self.open(entry, false)?;
        if file.metadata()?.len() > limit as u64 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(bytes)
    }
    pub fn atomic(&self, entry: &str, bytes: &[u8]) -> io::Result<()> {
        self.atomic_when(entry, bytes, || true)
    }
    pub fn atomic_when(
        &self,
        entry: &str,
        bytes: &[u8],
        mut allowed: impl FnMut() -> bool,
    ) -> io::Result<()> {
        let temporary = format!(".{}.tmp", uuid::Uuid::new_v4());
        let result = (|| {
            let mut file = self.open(&temporary, true)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            if !allowed() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let from = name(&temporary)?;
            let to = name(entry)?;
            if unsafe {
                libc::renameat(
                    self.file.as_raw_fd(),
                    from.as_ptr(),
                    self.file.as_raw_fd(),
                    to.as_ptr(),
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            self.sync()
        })();
        if result.is_err() {
            let _ = self.remove(&temporary);
        }
        result
    }
    pub fn remove(&self, entry: &str) -> io::Result<()> {
        let c = name(entry)?;
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), c.as_ptr(), 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
    pub fn blob(&self, bytes: &[u8]) -> io::Result<String> {
        if bytes.len() > FILE_LIMIT {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let id = blake3::hash(bytes).to_hex().to_string();
        match self.read(&id, FILE_LIMIT) {
            Ok(existing) if existing == bytes => {}
            Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => self.atomic(&id, bytes)?,
            Err(e) => return Err(e),
        }
        Ok(id)
    }
    pub fn read_blob(&self, id: &str) -> io::Result<Vec<u8>> {
        if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let bytes = self.read(id, FILE_LIMIT)?;
        if blake3::hash(&bytes).to_hex().as_str() != id {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(bytes)
    }
}
