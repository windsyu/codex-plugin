use super::*;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::os::{
    fd::{AsRawFd, FromRawFd},
    unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        process::{CommandExt, ExitStatusExt},
    },
};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

pub(super) struct Programs {
    pub rg: Option<PathBuf>,
    pub git: Option<PathBuf>,
}
impl Programs {
    pub fn discover() -> Self {
        Self {
            rg: resolve("rg"),
            git: resolve("git"),
        }
    }
}
fn resolve(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|p| p.is_absolute())
        .find_map(|dir| {
            let p = dir.join(name).canonicalize().ok()?;
            let m = p.metadata().ok()?;
            (m.is_file() && m.permissions().mode() & 0o111 != 0).then_some(p)
        })
}
pub(super) struct Output {
    pub bytes: Vec<u8>,
    pub code: Option<i32>,
    pub truncated: bool,
}
struct ChildGuard(Child, bool);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Each query owns its process group; extensions are disabled as well.
        if !self.1 {
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
pub(super) fn snapshot(bytes: &[u8]) -> Result<File> {
    prepare_snapshot(tempfile::tempfile()?, bytes)
}
// Only called with a new path inside a request-owned, mode-0700 TempDir.
// Git must stat a real file on macOS: concurrent /dev/fd/N lookups can fail
// even when fstat(N) succeeds, and Git can turn that failure into empty input.
pub(super) fn snapshot_directory() -> Result<tempfile::TempDir> {
    Ok(tempfile::Builder::new()
        .prefix("codex-workspace-diff-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?)
}
pub(super) fn snapshot_at(path: &Path, bytes: &[u8]) -> Result<File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    prepare_snapshot(file, bytes)
}
fn prepare_snapshot(mut file: File, bytes: &[u8]) -> Result<File> {
    file.write_all(bytes)?;
    file.rewind()?;
    // Own the eventual inherited descriptor before Command allocates any pipes.
    // A fixed dup2 target in pre_exec can overwrite Rust's exec-error pipe.
    let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 128) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if diagnostics_enabled() {
        eprintln!(
            "{}",
            json!({"source":"workspace_snapshot","event":"created","pid":std::process::id(),"expectedBytes":bytes.len(),"snapshot":fd_metadata(&file)})
        );
    }
    Ok(file)
}
pub(super) fn snapshot_path(file: &File) -> String {
    format!("/dev/fd/{}", file.as_raw_fd())
}
fn diagnostics_enabled() -> bool {
    std::env::var_os("CODEX_WORKSPACE_DIAGNOSTICS").is_some_and(|v| v == "1")
}
// No path, contents, content digest, argv or environment values are recorded.
pub(super) fn fd_metadata(file: &File) -> Value {
    let fd = file.as_raw_fd();
    let metadata = file.metadata();
    let offset = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) };
    let offset_errno = (offset < 0)
        .then(|| std::io::Error::last_os_error().raw_os_error())
        .flatten();
    let descriptor_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    let status_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    let m = metadata.as_ref().ok();
    json!({"fd":fd,"bytes":m.map(|m| m.len()),"offset":(offset >= 0).then_some(offset),
        "device":m.map(|m| m.dev()),"inode":m.map(|m| m.ino()),"mode":m.map(|m| m.mode()),
        "descriptorFlags":(descriptor_flags >= 0).then_some(descriptor_flags),
        "statusFlags":(status_flags >= 0).then_some(status_flags),
        "statErrno":metadata.err().and_then(|e| e.raw_os_error()),"offsetErrno":offset_errno})
}
#[derive(Default)]
struct Diagnostics {
    spawn_errno: Option<i32>,
    child_pid: Option<u32>,
    code: Option<i32>,
    signal: Option<i32>,
    stderr_bytes: usize,
    stderr_complete: bool,
    stderr_limited: bool,
    capture_stderr: bool,
    stderr: Vec<u8>,
}
fn stderr_categories(bytes: &[u8]) -> Vec<&'static str> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let lower = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    let mut classes: Vec<_> = [
        ("bad file descriptor", "bad_file_descriptor"),
        ("could not access", "could_not_access"),
        ("short read", "short_read"),
        ("cannot hash", "cannot_hash"),
        ("permission denied", "permission_denied"),
        ("no such file", "not_found"),
    ]
    .into_iter()
    .filter_map(|(pattern, class)| lower.contains(pattern).then_some(class))
    .collect();
    if classes.is_empty() {
        classes.push("unclassified");
    }
    classes
}
fn nonblocking(file: &impl AsRawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
pub(super) fn run(
    root: &fs::Root,
    program: &Path,
    args: &[String],
    files: &[File],
    budget: &Budget,
    limit: usize,
) -> Result<Output> {
    let enabled = diagnostics_enabled();
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = enabled.then(|| SEQUENCE.fetch_add(1, Ordering::Relaxed));
    if enabled {
        eprintln!(
            "{}",
            json!({"source":"workspace_process","event":"before_spawn","pid":std::process::id(),"operation":id,
            "inputs":files.iter().map(fd_metadata).collect::<Vec<_>>(),"mapping":"owned_fd_child_cloexec_clear"})
        );
    }
    let mut diagnostic = Diagnostics {
        capture_stderr: enabled,
        ..Diagnostics::default()
    };
    let result = run_inner(root, program, args, files, budget, limit, &mut diagnostic);
    if enabled {
        eprintln!(
            "{}",
            json!({"source":"workspace_process","event":"finished","pid":std::process::id(),"operation":id,
            "inputs":files.iter().map(fd_metadata).collect::<Vec<_>>(),"spawnErrno":diagnostic.spawn_errno,"childPid":diagnostic.child_pid,
            "exitCode":diagnostic.code,"signal":diagnostic.signal,"error":result.as_ref().err().map(|e| e.0),
            "stdoutBytes":result.as_ref().ok().map(|out| out.bytes.len()),"truncated":result.as_ref().ok().map(|out| out.truncated),
            "stderrBytes":diagnostic.stderr_bytes,"stderrComplete":diagnostic.stderr_complete,
            "stderrLimited":diagnostic.stderr_limited,"stderrCategories":stderr_categories(&diagnostic.stderr)})
        );
    }
    result
}
fn run_inner(
    root: &fs::Root,
    program: &Path,
    args: &[String],
    files: &[File],
    budget: &Budget,
    limit: usize,
    diagnostic: &mut Diagnostics,
) -> Result<Output> {
    budget.check()?;
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C")
        .env("HOME", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let root_fd = root.file.as_raw_fd();
    let inherited: Vec<_> = files.iter().map(AsRawFd::as_raw_fd).collect();
    unsafe {
        command.pre_exec(move || {
            if libc::fchdir(root_fd) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Descriptors are owned by the caller for this entire synchronous
            // run. Change only the fork child's flags; the parent remains
            // CLOEXEC even while other threads spawn unrelated processes.
            for fd in &inherited {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = ChildGuard(
        command.spawn().map_err(|e| {
            diagnostic.spawn_errno = e.raw_os_error();
            Fault("program_unavailable")
        })?,
        false,
    );
    diagnostic.child_pid = Some(child.0.id());
    let mut out = child.0.stdout.take().expect("pipe");
    let mut err = child.0.stderr.take().expect("pipe");
    nonblocking(&out)?;
    nonblocking(&err)?;
    let mut bytes = Vec::new();
    let mut eof = false;
    let mut err_eof = false;
    loop {
        budget.check()?;
        let mut buf = [0; 8192];
        for _ in 0..32 {
            match out.read(&mut buf) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    bytes.extend_from_slice(&buf[..n]);
                    if bytes.len() > limit {
                        bytes.truncate(limit);
                        return Ok(Output {
                            bytes,
                            code: None,
                            truncated: true,
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        for _ in 0..4 {
            match err.read(&mut buf) {
                Ok(0) => {
                    err_eof = true;
                    diagnostic.stderr_complete = true;
                    break;
                }
                Ok(n) => {
                    let remaining = 65536 - diagnostic.stderr_bytes;
                    diagnostic.stderr_bytes += n;
                    if diagnostic.capture_stderr {
                        diagnostic
                            .stderr
                            .extend_from_slice(&buf[..n.min(remaining)]);
                    }
                    if n > remaining {
                        diagnostic.stderr_limited = true;
                        return Err(Fault("program_output_limit"));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(status) = child.0.try_wait()?
            && eof
            && err_eof
        {
            child.1 = true;
            diagnostic.code = status.code();
            diagnostic.signal = status.signal();
            return Ok(Output {
                bytes,
                code: status.code(),
                truncated: false,
            });
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
