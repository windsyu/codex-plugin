//! Descriptor-relative discovery. No symlink component or arbitrary path is read.
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

pub(super) fn child(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
    if name.as_bytes().contains(&b'/') || name == ".." || name.is_empty() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let name = CString::new(name.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: parent is live, name is terminated, and the returned fd is owned once.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(file)
}

pub(super) fn root(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // The launcher supplies an explicit canonical home. Pin it before scanning.
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(path)
}

struct Directory {
    file: File,
    stream: *mut libc::DIR,
    depth: usize,
}
impl Directory {
    fn new(file: File, depth: usize) -> io::Result<Self> {
        use std::os::fd::IntoRawFd;
        // A fresh open description prevents readdir from moving the pinned fd's cursor.
        let fd = child(&file, OsStr::new("."), true)?.into_raw_fd();
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            unsafe {
                libc::close(fd);
            }
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            file,
            stream,
            depth,
        })
    }
    fn next(&mut self) -> io::Result<Option<OsString>> {
        loop {
            // SAFETY: stream is owned for this Directory's lifetime; copy d_name
            // before calling readdir again. Enumeration never reads file content.
            // Supported hosts are macOS and Linux. errno distinguishes end of
            // directory from an I/O error; stale errno must not mask either.
            #[cfg(target_os = "macos")]
            let errno = unsafe { libc::__error() };
            #[cfg(not(target_os = "macos"))]
            let errno = unsafe { libc::__errno_location() };
            unsafe {
                *errno = 0;
            }
            let entry = unsafe { libc::readdir(self.stream) };
            if entry.is_null() {
                let code = unsafe { *errno };
                return if code == 0 {
                    Ok(None)
                } else {
                    Err(io::Error::from_raw_os_error(code))
                };
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                return Ok(Some(OsString::from_vec(name.to_vec())));
            }
        }
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.stream);
        }
    }
}

pub(super) struct Scan {
    stack: Vec<Directory>,
}
impl Scan {
    pub fn new(home: &File) -> io::Result<Self> {
        let mut stack = Vec::new();
        for name in ["archived_sessions", "sessions"] {
            match child(home, OsStr::new(name), true) {
                Ok(file) => stack.push(Directory::new(file, 0)?),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(Self { stack })
    }
    /// At most budget directory entries per tick, without restarting a large scan.
    pub fn step(
        &mut self,
        budget: usize,
        mut candidate: impl FnMut(&File, &OsStr),
    ) -> io::Result<bool> {
        for _ in 0..budget {
            let Some(directory) = self.stack.last_mut() else {
                return Ok(true);
            };
            let Some(name) = directory.next()? else {
                self.stack.pop();
                continue;
            };
            if name.as_bytes().ends_with(b".jsonl") {
                candidate(&directory.file, &name);
            } else if directory.depth < 4
                && let Ok(file) = child(&directory.file, &name, true)
                && let Ok(next) = Directory::new(file, directory.depth + 1)
            {
                self.stack.push(next);
            }
        }
        Ok(self.stack.is_empty())
    }
}
