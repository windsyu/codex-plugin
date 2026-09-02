use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub const MIN_TERMINAL_ROWS: u16 = 2;
pub const MAX_TERMINAL_ROWS: u16 = 300;
pub const MIN_TERMINAL_COLS: u16 = 10;
pub const MAX_TERMINAL_COLS: u16 = 500;

#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub executable: PathBuf,
    pub argv: Vec<OsString>,
    pub canonical_cwd: PathBuf,
    pub env_allowlist: BTreeMap<OsString, OsString>,
    pub rows: u16,
    pub cols: u16,
}

impl SpawnSpec {
    pub fn validate(mut self) -> Result<Self> {
        if !valid_terminal_size(self.rows, self.cols) {
            anyhow::bail!("terminal size is outside the supported bounds");
        }
        if !self.canonical_cwd.is_absolute() {
            anyhow::bail!("cwd must be an absolute path");
        }
        self.canonical_cwd = fs::canonicalize(&self.canonical_cwd).with_context(|| {
            format!(
                "canonicalize Session Worker cwd {}",
                self.canonical_cwd.display()
            )
        })?;
        if !self.canonical_cwd.is_dir() {
            anyhow::bail!("cwd must be a directory");
        }
        self.executable = canonical_executable(&self.executable)?;
        for key in self.env_allowlist.keys() {
            let Some(key) = key.to_str() else {
                anyhow::bail!("environment key must be UTF-8");
            };
            if !matches!(
                key,
                "TERM"
                    | "COLORTERM"
                    | "LANG"
                    | "LC_ALL"
                    | "PATH"
                    | "HOME"
                    | "USER"
                    | "TMPDIR"
                    | "CODEX_HOME"
            ) {
                anyhow::bail!("environment key {key} is not allowed for Session Worker");
            }
        }
        Ok(self)
    }
}

pub fn valid_terminal_size(rows: u16, cols: u16) -> bool {
    (MIN_TERMINAL_ROWS..=MAX_TERMINAL_ROWS).contains(&rows)
        && (MIN_TERMINAL_COLS..=MAX_TERMINAL_COLS).contains(&cols)
}

pub fn canonical_executable(path: &Path) -> Result<PathBuf> {
    let path = if path.components().count() == 1 {
        find_in_path(path.as_os_str()).context("Session Kernel CLI is not on PATH")?
    } else {
        path.to_path_buf()
    };
    let path = fs::canonicalize(&path)
        .with_context(|| format!("canonicalize Session Kernel executable {}", path.display()))?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() {
        anyhow::bail!("Session Kernel executable must be a regular file");
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o111 == 0 {
        anyhow::bail!("Session Kernel executable is not executable");
    }
    Ok(path)
}

pub fn find_in_path(executable: &OsStr) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|path| path.join(executable))
        .find(|candidate| {
            fs::metadata(candidate).is_ok_and(|metadata| {
                if !metadata.is_file() {
                    return false;
                }
                #[cfg(unix)]
                return metadata.permissions().mode() & 0o111 != 0;
                #[cfg(not(unix))]
                true
            })
        })
}

pub(super) struct PtyProcess {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    reader: Option<Box<dyn Read + Send>>,
    process_group: Option<i32>,
}

impl PtyProcess {
    pub fn spawn(spec: SpawnSpec) -> Result<Self> {
        let spec = spec.validate()?;
        let pair = native_pty_system().openpty(PtySize {
            rows: spec.rows,
            cols: spec.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let mut command = CommandBuilder::new(&spec.executable);
        command.env_clear();
        command.args(&spec.argv);
        command.cwd(&spec.canonical_cwd);
        #[cfg(unix)]
        command.umask(Some(0o077));
        for (key, value) in spec.env_allowlist {
            command.env(key, value);
        }
        let child = pair.slave.spawn_command(command).with_context(|| {
            format!(
                "spawn Session Worker executable {}",
                spec.executable.display()
            )
        })?;
        let process_group = {
            #[cfg(unix)]
            {
                pair.master.process_group_leader()
            }
            #[cfg(not(unix))]
            {
                None
            }
        };
        drop(pair.slave);
        Ok(Self {
            master: pair.master,
            writer,
            child,
            reader: Some(reader),
            process_group,
        })
    }

    pub fn take_reader(&mut self) -> Result<Box<dyn Read + Send>> {
        self.reader.take().context("PTY reader was already taken")
    }

    pub fn write_input(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        if !valid_terminal_size(rows, cols) {
            anyhow::bail!("terminal size is outside the supported bounds");
        }
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
    }

    pub fn try_wait(&mut self) -> Result<Option<portable_pty::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    pub fn terminate(&mut self) -> Result<()> {
        #[cfg(unix)]
        if let Some(group) = self.process_group {
            // SAFETY: `group` is returned by the owned PTY. A negative pid targets only
            // that process group, never an unresolved environment value or broad target.
            let result = unsafe { libc::kill(-group, libc::SIGTERM) };
            if result == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        self.child.kill()?;
        Ok(())
    }

    pub fn force_kill(&mut self) -> Result<()> {
        #[cfg(unix)]
        if let Some(group) = self.process_group {
            // SAFETY: the PTY owns this process group; SIGKILL is the bounded
            // escalation after the graceful group-wide SIGTERM deadline.
            let result = unsafe { libc::kill(-group, libc::SIGKILL) };
            if result == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        self.child.kill()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn env() -> BTreeMap<OsString, OsString> {
        BTreeMap::from([(OsString::from("TERM"), OsString::from("xterm-256color"))])
    }

    #[test]
    fn argv_is_not_interpreted_by_a_shell_and_environment_is_allowlisted() -> Result<()> {
        let temp = TempDir::new()?;
        let marker = temp.path().join("must-not-exist");
        let mut process = PtyProcess::spawn(SpawnSpec {
            executable: PathBuf::from("/bin/echo"),
            argv: vec![OsString::from(format!("$(touch {})", marker.display()))],
            canonical_cwd: temp.path().to_path_buf(),
            env_allowlist: env(),
            rows: 24,
            cols: 80,
        })?;
        let mut reader = process.take_reader()?;
        let mut output = Vec::new();
        reader.read_to_end(&mut output)?;
        assert!(String::from_utf8_lossy(&output).contains("$(touch"));
        assert!(!marker.exists());
        Ok(())
    }

    #[test]
    fn rejects_invalid_cwd_size_and_secret_environment_keys() {
        let mut spec = SpawnSpec {
            executable: PathBuf::from("/bin/echo"),
            argv: Vec::new(),
            canonical_cwd: PathBuf::from("relative"),
            env_allowlist: env(),
            rows: 1,
            cols: 80,
        };
        assert!(spec.clone().validate().is_err());
        spec.rows = 24;
        assert!(spec.clone().validate().is_err());
        spec.canonical_cwd = std::env::temp_dir();
        spec.env_allowlist
            .insert(OsString::from("OPENAI_API_KEY"), OsString::from("secret"));
        assert!(spec.validate().is_err());
    }
}
