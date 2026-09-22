use super::*;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::os::{
    fd::{AsRawFd, FromRawFd},
    unix::{fs::PermissionsExt, process::CommandExt},
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
    let mut file = tempfile::tempfile()?;
    file.write_all(bytes)?;
    file.rewind()?;
    Ok(file)
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
    // Keep source descriptors above the fixed inherited range (64..96).
    let duplicates: Vec<File> = files
        .iter()
        .map(|f| {
            let fd = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 128) };
            if fd < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(unsafe { File::from_raw_fd(fd) })
            }
        })
        .collect::<std::io::Result<_>>()?;
    unsafe {
        command.pre_exec(move || {
            if libc::fchdir(root_fd) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            for (i, file) in duplicates.iter().enumerate() {
                if libc::dup2(file.as_raw_fd(), 64 + i as i32) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = ChildGuard(
        command.spawn().map_err(|_| Fault("program_unavailable"))?,
        false,
    );
    let mut out = child.0.stdout.take().expect("pipe");
    let mut err = child.0.stderr.take().expect("pipe");
    nonblocking(&out)?;
    nonblocking(&err)?;
    let mut bytes = Vec::new();
    let mut discarded = 0;
    let mut eof = false;
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
                Ok(0) => break,
                Ok(n) => {
                    discarded += n;
                    if discarded > 65536 {
                        return Err(Fault("program_output_limit"));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(status) = child.0.try_wait()?
            && eof
        {
            child.1 = true;
            return Ok(Output {
                bytes,
                code: status.code(),
                truncated: false,
            });
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
