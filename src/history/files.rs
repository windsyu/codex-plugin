use std::ffi::CString;
use std::fs::{self, File, Metadata};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
use std::path::{Path, PathBuf};

pub(super) fn signature(meta: &Metadata) -> [u64; 7] {
    [
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime() as u64,
        meta.mtime_nsec() as u64,
        meta.ctime() as u64,
        meta.ctime_nsec() as u64,
    ]
}

pub(super) fn regular(path: &Path) -> std::io::Result<Metadata> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.nlink() != 1 || meta.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    Ok(meta)
}

pub(super) fn source_signature(path: &Path) -> std::io::Result<Vec<[u64; 7]>> {
    let mut result = vec![signature(&regular(path)?)];
    for suffix in ["-wal", "-journal"] {
        let mut auxiliary = path.as_os_str().to_os_string();
        auxiliary.push(suffix);
        match regular(Path::new(&auxiliary)) {
            Ok(meta) => result.push(signature(&meta)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => result.push([0; 7]),
            Err(error) => return Err(error),
        }
    }
    // SHM is not source content and contains transient locks/read marks. Check
    // its path type, but do not invalidate pagination on external read marks.
    let mut shm = path.as_os_str().to_os_string();
    shm.push("-shm");
    match regular(Path::new(&shm)) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(result)
}

pub(super) struct BlobRoot {
    path: PathBuf,
    file: File,
}
impl BlobRoot {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        let path = path.canonicalize()?;
        let name = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: owned NUL-terminated name; returned descriptor is owned once.
        let fd = unsafe {
            libc::open(
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            path,
            file: unsafe { File::from_raw_fd(fd) },
        })
    }
    pub fn read_file(&self, relative: &str) -> std::io::Result<File> {
        let parts: Vec<_> = relative.split('/').collect();
        if parts.len() > 16
            || relative.len() > 1024
            || relative.contains(['\0', '\\'])
            || parts
                .iter()
                .any(|p| p.is_empty() || *p == "." || *p == "..")
        {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        let current = fs::symlink_metadata(&self.path)?;
        let pinned = self.file.metadata()?;
        if !current.is_dir() || current.dev() != pinned.dev() || current.ino() != pinned.ino() {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        let mut parent = self.file.try_clone()?;
        for (i, part) in parts.iter().enumerate() {
            let directory = i + 1 < parts.len();
            let name = CString::new(*part)?;
            let flags = libc::O_RDONLY
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | libc::O_CLOEXEC
                | if directory { libc::O_DIRECTORY } else { 0 };
            // SAFETY: parent stays alive during openat. No following of links
            // at any component, no FIFO blocking, no pathname reopen of file.
            let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            parent = unsafe { File::from_raw_fd(fd) };
            let meta = parent.metadata()?;
            if meta.uid() != unsafe { libc::geteuid() }
                || if directory {
                    !meta.is_dir()
                } else {
                    !meta.is_file() || meta.nlink() != 1
                }
            {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
        }
        Ok(parent)
    }
}
