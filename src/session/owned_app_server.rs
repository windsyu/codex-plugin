use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::permissions::{prepare_private_dir, validate_private_unix_socket};

const GUARD_GRACE: Duration = Duration::from_secs(1);

pub(super) struct OwnedAppServer {
    guard: Child,
    keepalive: Option<ChildStdin>,
    socket_path: PathBuf,
}

impl OwnedAppServer {
    pub(super) fn spawn(
        codex_executable: &Path,
        cwd: &Path,
        socket_path: PathBuf,
        environment: BTreeMap<OsString, OsString>,
    ) -> Result<Self> {
        let parent = socket_path
            .parent()
            .context("owned App Server socket has no parent")?;
        prepare_private_dir(parent, "owned App Server runtime")?;
        if socket_path.exists() {
            anyhow::bail!("owned App Server socket already exists");
        }
        let observer = std::env::current_exe().context("resolve Observer executable")?;
        let mut command = Command::new(observer);
        command
            .env_clear()
            .envs(environment)
            .arg("owned-app-server-guard")
            .arg("--codex-executable")
            .arg(codex_executable)
            .arg("--cwd")
            .arg(cwd)
            .arg("--socket")
            .arg(&socket_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut guard = command.spawn().context("spawn owned App Server guard")?;
        let keepalive = guard
            .stdin
            .take()
            .context("owned App Server guard stdin unavailable")?;
        Ok(Self {
            guard,
            keepalive: Some(keepalive),
            socket_path,
        })
    }

    pub(super) fn wait_ready(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.guard.try_wait()? {
                anyhow::bail!("owned App Server exited before readiness: {status}");
            }
            if self.socket_path.exists() {
                validate_private_unix_socket(&self.socket_path, "owned app-server endpoint")?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                anyhow::bail!("owned App Server readiness timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub(super) fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.guard.try_wait()?)
    }

    pub(super) fn terminate(&mut self) {
        self.keepalive.take();
    }

    pub(super) fn stop_and_reap(&mut self, grace: Duration) {
        self.terminate();
        let deadline = Instant::now() + grace;
        loop {
            match self.guard.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => break,
            }
        }
        let _ = self.guard.kill();
        let _ = self.guard.wait();
    }
}

impl Drop for OwnedAppServer {
    fn drop(&mut self) {
        self.stop_and_reap(GUARD_GRACE + GUARD_GRACE);
    }
}

pub(crate) fn run_guard(codex_executable: &Path, cwd: &Path, socket: &Path) -> Result<()> {
    let executable = super::pty::canonical_executable(codex_executable)?;
    let cwd = std::fs::canonicalize(cwd).context("canonicalize guard cwd")?;
    if !cwd.is_dir() {
        anyhow::bail!("guard cwd is not a directory");
    }
    let parent = socket.parent().context("guard socket has no parent")?;
    prepare_private_dir(parent, "owned App Server runtime")?;
    if socket.exists() {
        anyhow::bail!("guard socket already exists");
    }
    let _runtime_cleanup = GuardRuntimeCleanup(socket.to_path_buf());

    let mut command = Command::new(executable);
    command
        .env_clear()
        .envs(inherited_allowlisted_environment())
        .args(owned_app_server_argv(socket))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut app_server = command.spawn().context("spawn Codex App Server")?;
    let (closed_tx, closed_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buffer = [0_u8; 64];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = closed_tx.send(());
    });

    loop {
        if let Some(status) = app_server.try_wait()? {
            if status.success() {
                return Ok(());
            }
            anyhow::bail!("Codex App Server exited: {status}");
        }
        if closed_rx.try_recv().is_ok() {
            terminate_child(&mut app_server, GUARD_GRACE);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct GuardRuntimeCleanup(PathBuf);

impl Drop for GuardRuntimeCleanup {
    fn drop(&mut self) {
        if let Err(error) = super::runtime_dir::cleanup_guard_worker_dir(&self.0)
            && error
                .downcast_ref::<std::io::Error>()
                .is_none_or(|error| error.kind() != std::io::ErrorKind::NotFound)
        {
            tracing::warn!(path = %self.0.display(), error = %format!("{error:#}"), "owned App Server guard failed to clean Worker runtime");
        }
    }
}

pub(super) fn owned_app_server_argv(socket: &Path) -> [OsString; 3] {
    let mut endpoint = OsString::from("unix://");
    endpoint.push(socket.as_os_str());
    [
        OsString::from("app-server"),
        OsString::from("--listen"),
        endpoint,
    ]
}

fn inherited_allowlisted_environment() -> BTreeMap<OsString, OsString> {
    [
        "TERM",
        "COLORTERM",
        "LANG",
        "LC_ALL",
        "PATH",
        "HOME",
        "USER",
        "TMPDIR",
        "CODEX_HOME",
    ]
    .into_iter()
    .filter_map(|key| std::env::var_os(key).map(|value| (OsString::from(key), value)))
    .collect()
}

fn terminate_child(child: &mut Child, grace: Duration) {
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: the pid comes from this exact owned Child and is never user supplied.
            let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
    #[cfg(not(unix))]
    let _ = child.kill();

    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_server_argv_is_fixed_and_does_not_use_a_shell() {
        assert_eq!(
            owned_app_server_argv(Path::new("/private/runtime/upstream.sock")),
            [
                OsString::from("app-server"),
                OsString::from("--listen"),
                OsString::from("unix:///private/runtime/upstream.sock"),
            ]
        );
    }

    #[test]
    fn guard_environment_is_closed() {
        let allowed = inherited_allowlisted_environment();
        assert!(allowed.keys().all(|key| matches!(
            key.to_str(),
            Some(
                "TERM"
                    | "COLORTERM"
                    | "LANG"
                    | "LC_ALL"
                    | "PATH"
                    | "HOME"
                    | "USER"
                    | "TMPDIR"
                    | "CODEX_HOME"
            )
        )));
    }

    #[test]
    fn endpoint_preserves_non_utf8_paths_without_shell_interpretation() {
        let argv = owned_app_server_argv(Path::new("/tmp/$(touch must-not-run).sock"));
        assert_eq!(
            argv[2],
            std::ffi::OsStr::new("unix:///tmp/$(touch must-not-run).sock")
        );
    }
}
