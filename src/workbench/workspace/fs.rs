use super::*;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};

pub(super) struct Root {
    pub file: File,
}

pub(super) fn permitted(path: &str) -> bool {
    path.split('/').all(|name| {
        let n = name.to_ascii_lowercase();
        !matches!(
            n.as_str(),
            ".git"
                | ".ssh"
                | ".aws"
                | ".azure"
                | ".kube"
                | ".gnupg"
                | ".codex"
                | ".codex-web"
                | ".docker"
                | ".netrc"
                | ".npmrc"
                | ".pypirc"
                | "auth.json"
                | "credentials.json"
                | "credentials"
                | "secrets.json"
                | "secrets.yaml"
                | "secrets.yml"
        ) && !n.starts_with(".env")
            && !n.starts_with("id_rsa")
            && !n.starts_with("id_ed25519")
            && !n.starts_with("id_ecdsa")
            && !n.starts_with("id_dsa")
            && !n.starts_with("credentials.")
            && !n.starts_with("secrets.")
            && ![".pem", ".key", ".p12", ".pfx", ".jks", ".keystore"]
                .iter()
                .any(|ext| n.ends_with(ext))
    })
}
pub(super) fn validate(path: &str, root_allowed: bool) -> Result<()> {
    if path.is_empty() && root_allowed {
        return Ok(());
    }
    if path.is_empty()
        || path.len() > 4096
        || path.split('/').count() > 64
        || path.contains(['\0', '\\'])
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        || !permitted(path)
    {
        return Err(Fault("forbidden_path"));
    }
    Ok(())
}
fn open_at(parent: &File, name: &str, directory: bool) -> Result<File> {
    let name = CString::new(name).map_err(|_| Fault("forbidden_path"))?;
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        return Err(
            if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
                Fault("forbidden_path")
            } else {
                e.into()
            },
        );
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let meta = file.metadata()?;
    if if directory {
        !meta.is_dir()
    } else {
        !meta.is_file() || meta.nlink() != 1
    } {
        return Err(Fault("forbidden_path"));
    }
    Ok(file)
}
impl Root {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let name = CString::new(path.as_os_str().as_bytes())?;
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
            file: unsafe { File::from_raw_fd(fd) },
        })
    }
    fn directory(&self, path: &str) -> Result<File> {
        validate(path, true)?;
        // openat(".") gives each listing its own directory offset.
        let mut dir = open_at(&self.file, ".", true)?;
        if !path.is_empty() {
            for part in path.split('/') {
                dir = open_at(&dir, part, true)?;
            }
        }
        Ok(dir)
    }
    pub fn file(&self, path: &str) -> Result<File> {
        validate(path, false)?;
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        open_at(&self.directory(parent)?, name, false)
    }
    // Blob-only Git reads may refer to a deleted path. Existing ancestors still
    // must be directories, never links; no worktree read uses this exception.
    pub fn git_path(&self, path: &str) -> Result<()> {
        validate(path, false)?;
        let mut dir = open_at(&self.file, ".", true)?;
        let parts: Vec<_> = path.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            match open_at(&dir, part, i + 1 < parts.len()) {
                Ok(f) => dir = f,
                Err(Fault("not_found")) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    pub fn read(&self, path: &str) -> Result<String> {
        let file = self.file(path)?;
        let before = file.metadata()?;
        if before.len() > FILE_LIMIT as u64 {
            return Err(Fault("file_too_large"));
        }
        let mut bytes = Vec::new();
        (&file)
            .take(FILE_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        if bytes.len() > FILE_LIMIT {
            return Err(Fault("file_too_large"));
        }
        if before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return Err(Fault("file_changed"));
        }
        text(bytes)
    }
    pub fn list(&self, path: &str, cursor: Option<&str>, budget: &Budget) -> Result<Value> {
        let dir = self.directory(path)?;
        let before = dir.metadata()?;
        let duplicate = unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if duplicate < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let pointer = unsafe { libc::fdopendir(duplicate) };
        if pointer.is_null() {
            unsafe {
                libc::close(duplicate);
            }
            return Err(Fault("workspace_io_error"));
        }
        struct Directory(*mut libc::DIR);
        impl Drop for Directory {
            fn drop(&mut self) {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
        let directory = Directory(pointer);
        let mut entries = Vec::<Value>::new();
        let mut omitted = 0;
        let mut scanned = 0;
        let mut name_bytes = 0;
        loop {
            budget.check()?;
            unsafe {
                *errno_pointer() = 0;
            }
            let entry = unsafe { libc::readdir(directory.0) };
            if entry.is_null() {
                if unsafe { *errno_pointer() } != 0 {
                    return Err(Fault("workspace_io_error"));
                }
                break;
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            scanned += 1;
            name_bytes += bytes.len();
            if scanned > 10000 || name_bytes > FILE_LIMIT {
                return Err(Fault("directory_too_large"));
            }
            let Ok(name) = std::str::from_utf8(bytes) else {
                omitted += 1;
                continue;
            };
            let relative = if path.is_empty() {
                name.to_string()
            } else {
                format!("{path}/{name}")
            };
            if validate(&relative, false).is_err() {
                omitted += 1;
                continue;
            }
            let kind = if open_at(&dir, name, true).is_ok() {
                "directory"
            } else if open_at(&dir, name, false).is_ok() {
                "file"
            } else {
                "unavailable"
            };
            entries.push(json!({"name":name,"path":relative,"kind":kind}));
        }
        let after = dir.metadata()?;
        if before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return Err(Fault("workspace_changed"));
        }
        entries.sort_by(|a, b| {
            (a["kind"] != "directory")
                .cmp(&(b["kind"] != "directory"))
                .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
        });
        let fingerprint =
            blake3::hash(&serde_json::to_vec(&(path, &entries, omitted)).expect("directory JSON"))
                .to_hex()
                .to_string();
        let offset = page_offset(cursor, &fingerprint, PAGE)?;
        if offset > entries.len() {
            return Err(Fault("invalid_cursor"));
        }
        Ok(
            json!({"path":path,"entries":entries.iter().skip(offset).take(PAGE).collect::<Vec<_>>(),"omitted":omitted,"nextCursor":(offset + PAGE < entries.len()).then(|| format!("{fingerprint}:{}",offset + PAGE)),"truncated":false}),
        )
    }
}
pub(super) fn text(bytes: Vec<u8>) -> Result<String> {
    if bytes.contains(&0) {
        return Err(Fault("binary_file"));
    }
    String::from_utf8(bytes).map_err(|_| Fault("binary_file"))
}
pub(super) fn page_offset(cursor: Option<&str>, fingerprint: &str, size: usize) -> Result<usize> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let (hash, offset) = cursor.split_once(':').ok_or(Fault("invalid_cursor"))?;
    let offset: usize = offset.parse().map_err(|_| Fault("invalid_cursor"))?;
    if hash.len() != 64
        || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        || offset == 0
        || !offset.is_multiple_of(size)
        || offset > 10000
    {
        return Err(Fault("invalid_cursor"));
    }
    if hash != fingerprint {
        return Err(Fault("workspace_changed"));
    }
    Ok(offset)
}

// libc exposes errno as a thread-local pointer on the supported Unix hosts.
#[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
unsafe fn errno_pointer() -> *mut libc::c_int {
    unsafe { libc::__error() }
}
#[cfg(not(any(target_vendor = "apple", target_os = "freebsd")))]
unsafe fn errno_pointer() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}
