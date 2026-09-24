//! Workbench-owned settings. No native config is copied or written here.
//! All runtime I/O is confined to a bounded independent worker.
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use super::paths::WorkbenchPaths;
use super::recording::fs::Directory;
use serde::Serialize;
use tokio::sync::oneshot;
mod model;
pub use model::{Config, Overrides};
pub const LIMIT: usize = 64 * 1024;
pub const SCHEMA: &str = include_str!("../../docs/configuration/workbench.config.schema.json");

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigError {
    pub code: &'static str,
    pub field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
}
impl ConfigError {
    pub(crate) fn field(field: &str, code: &'static str) -> Self {
        Self {
            code,
            field: field.into(),
            line: None,
            column: None,
        }
    }
    fn io(_: io::Error) -> Self {
        Self::field("$", "config_io_error")
    }
}
impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}", self.code, self.field)?;
        if let Some(line) = self.line {
            write!(f, " (line {line}, column {})", self.column.unwrap_or(0))?;
        }
        Ok(())
    }
}
impl std::error::Error for ConfigError {}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub schema_version: u32,
    pub revision: Option<String>,
    pub saved: Option<Config>,
    pub effective: Config,
    pub defaults: Config,
    pub cli_overrides: Vec<&'static str>,
    pub config_path: PathBuf,
    pub effective_data_dir: PathBuf,
    pub restart_required: Vec<&'static str>,
    pub errors: Vec<ConfigError>,
    pub capabilities: Capabilities,
}
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub manual_cleanup: bool,
    pub retention: bool,
}

pub struct Prepared {
    directory: Directory,
    home: PathBuf,
    paths: WorkbenchPaths,
    cwd: PathBuf,
    overrides: Overrides,
    pub effective: Config,
    pub data_dir: PathBuf,
    application: bool,
}

// Resolve existing parents, but do not require the future storage directory to
// exist. Paths never become shell strings. Leaf symlinks are rejected by I/O.
pub(crate) fn location(path: &Path, cwd: &Path) -> io::Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut clean = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            _ => clean.push(c.as_os_str()),
        }
    }
    let mut parent = clean.as_path();
    let mut rest = Vec::new();
    while !parent.try_exists()? {
        rest.push(parent.file_name().ok_or(io::ErrorKind::InvalidInput)?);
        parent = parent.parent().ok_or(io::ErrorKind::InvalidInput)?;
    }
    let mut result = parent.canonicalize()?;
    for c in rest.into_iter().rev() {
        result.push(c);
    }
    Ok(result)
}
/// Resolve a launch identity without requiring a native directory to exist.
pub fn location_for_application(path: &Path, cwd: &Path) -> io::Result<PathBuf> {
    location(path, cwd)
}
fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
impl Prepared {
    pub fn load(
        home: &Path,
        cwd: &Path,
        paths: &WorkbenchPaths,
        config_dir: Option<&Path>,
        overrides: Overrides,
    ) -> Result<Self, ConfigError> {
        Self::load_inner(home, cwd, paths, config_dir, overrides, false)
    }
    /// Application settings do not require an installed CLI or native home.
    pub fn load_application(
        home: &Path,
        cwd: &Path,
        paths: &WorkbenchPaths,
        config_dir: Option<&Path>,
        overrides: Overrides,
    ) -> Result<Self, ConfigError> {
        Self::load_inner(home, cwd, paths, config_dir, overrides, true)
    }
    fn load_inner(
        home: &Path,
        cwd: &Path,
        paths: &WorkbenchPaths,
        config_dir: Option<&Path>,
        mut overrides: Overrides,
        application: bool,
    ) -> Result<Self, ConfigError> {
        let home = if application {
            location(home, cwd)
        } else {
            home.canonicalize()
        }
        .map_err(ConfigError::io)?;
        let cwd = cwd.canonicalize().map_err(ConfigError::io)?;
        if let Some(p) = &overrides.codex_bin
            && p != Path::new("codex")
        {
            overrides.codex_bin = Some(location(p, &cwd).map_err(ConfigError::io)?);
        }
        if let Some(p) = &overrides.data_dir {
            overrides.data_dir = Some(location(p, &cwd).map_err(ConfigError::io)?);
        }
        let requested = config_dir
            .map(|p| {
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    cwd.join(p)
                }
            })
            .unwrap_or_else(|| paths.config_dir());
        // Refuse a symlink leaf even when its destination has private permissions.
        if std::fs::symlink_metadata(&requested).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(ConfigError::field("$", "unsafe_config_directory"));
        }
        let path = location(&requested, &cwd).map_err(ConfigError::io)?;
        let initial_data = overrides
            .data_dir
            .clone()
            .unwrap_or_else(|| paths.history_dir());
        if path == home
            || path == cwd
            || overlap(&path, &initial_data)
            || overlap(&path, &home.join("sessions"))
            || overlap(&path, &home.join("archived_sessions"))
        {
            return Err(ConfigError::field("$", "unsafe_config_directory"));
        }
        if config_dir.is_none() {
            Directory::root(&paths.root).map_err(ConfigError::io)?;
        }
        let directory = Directory::root(&path).map_err(ConfigError::io)?;
        let lock = lock(&directory)?;
        let bytes = match directory.read("config.json", LIMIT) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let bytes = encoded(&Config::default())?;
                let mut file = directory
                    .open("config.json", true)
                    .map_err(ConfigError::io)?;
                file.write_all(&bytes).map_err(ConfigError::io)?;
                file.sync_all().map_err(ConfigError::io)?;
                directory.sync().map_err(ConfigError::io)?;
                bytes
            }
            Err(e) => return Err(ConfigError::io(e)),
        };
        let saved = match Config::parse(&bytes) {
            Ok(config) => config,
            Err(_) if application => Config::default(), // snapshot exposes the original error; never overwrite it
            Err(error) => return Err(error),
        };
        let effective = overrides.apply(&saved);
        if effective.storage.data_dir.is_none()
            && std::fs::symlink_metadata(&paths.root).is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(ConfigError::field(
                "storage.dataDir",
                "unsafe_data_directory",
            ));
        }
        let data_dir = location(
            effective
                .storage
                .data_dir
                .as_deref()
                .unwrap_or(&initial_data),
            &cwd,
        )
        .map_err(ConfigError::io)?;
        let mut prepared = Self {
            directory,
            home,
            paths: paths.clone(),
            cwd,
            overrides,
            effective,
            data_dir,
            application,
        };
        if let Err(error) = prepared.validate(&saved) {
            if application && error.field == "history.library" {
                prepared.effective.history.library.enabled = false;
            } else {
                return Err(error);
            }
        }
        prepared.effective.validate()?;
        prepared.validate(&prepared.effective)?;
        // Schema is app-owned; reject an unsafe replacement before writing it.
        match prepared.directory.read("config.schema.json", LIMIT) {
            Ok(old) if old == SCHEMA.as_bytes() => {}
            Ok(_) => prepared
                .directory
                .atomic("config.schema.json", SCHEMA.as_bytes())
                .map_err(ConfigError::io)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => prepared
                .directory
                .atomic("config.schema.json", SCHEMA.as_bytes())
                .map_err(ConfigError::io)?,
            Err(e) => return Err(ConfigError::io(e)),
        }
        drop(lock);
        Ok(prepared)
    }
    pub fn config_dir(&self) -> &Path {
        &self.directory.path
    }
    fn validate(&self, config: &Config) -> Result<(), ConfigError> {
        config.validate()?;
        let data = location(
            config
                .storage
                .data_dir
                .as_deref()
                .unwrap_or(&self.paths.history_dir()),
            &self.cwd,
        )
        .map_err(|_| ConfigError::field("storage.dataDir", "invalid_path"))?;
        if data == self.home
            || data == self.cwd
            || overlap(&data, &self.directory.path)
            || overlap(&data, &self.home.join("sessions"))
            || overlap(&data, &self.home.join("archived_sessions"))
        {
            return Err(ConfigError::field(
                "storage.dataDir",
                "unsafe_data_directory",
            ));
        }
        config
            .history
            .library
            .resolved(&self.home, &self.data_dir)
            .map_err(|code| ConfigError::field("history.library", code))?;
        if !self.application && config.launch.codex_bin != "codex" {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&config.launch.codex_bin)
                .map_err(|_| ConfigError::field("launch.codexBin", "executable_unavailable"))?;
            if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
                return Err(ConfigError::field(
                    "launch.codexBin",
                    "executable_unavailable",
                ));
            }
        }
        Ok(())
    }
    fn read(&self) -> Result<(Config, String), ConfigError> {
        self.directory.verify_location().map_err(ConfigError::io)?;
        let bytes = self
            .directory
            .read("config.json", LIMIT)
            .map_err(ConfigError::io)?;
        let config = Config::parse(&bytes)?;
        self.validate(&config)?;
        Ok((config, blake3::hash(&bytes).to_hex().to_string()))
    }
    fn snapshot(&self) -> Settings {
        let mut effective = self.effective.clone();
        let (saved, revision, errors) = match self.read() {
            Ok((c, r)) => {
                effective.history = c.history.clone();
                (Some(c), Some(r), vec![])
            }
            Err(e) => {
                effective.history.library.enabled = false;
                effective.history.cleanup.enabled = false;
                effective.history.cleanup.retention.enabled = false;
                (None, None, vec![e])
            }
        };
        let mut restart_required = vec![];
        if let Some(saved) = &saved {
            let future = self.overrides.apply(saved);
            if future.launch.open_browser != effective.launch.open_browser {
                restart_required.push("launch.openBrowser");
            }
            if future.launch.codex_bin != effective.launch.codex_bin {
                restart_required.push("launch.codexBin");
            }
            if future.launch.profile != effective.launch.profile {
                restart_required.push("launch.profile");
            }
            if future.launch.provider_profile != effective.launch.provider_profile {
                restart_required.push("launch.providerProfile");
            }
            if future.storage != effective.storage {
                restart_required.push("storage.dataDir");
            }
        }
        Settings {
            schema_version: 2,
            revision,
            saved,
            effective,
            defaults: Config::default(),
            cli_overrides: self.overrides.fields(),
            config_path: self.directory.path.join("config.json"),
            effective_data_dir: self.data_dir.clone(),
            restart_required,
            errors,
            capabilities: Capabilities {
                manual_cleanup: true,
                retention: true,
            },
        }
    }
    fn save(
        &self,
        revision: &str,
        config: &Config,
        deadline: Instant,
    ) -> Result<Settings, ConfigError> {
        let mut upgraded = config.clone();
        upgraded.schema_version = 2;
        let config = &upgraded;
        self.validate(config)?;
        self.directory.verify_location().map_err(ConfigError::io)?;
        let lock = lock(&self.directory)?;
        let old = self
            .directory
            .read("config.json", LIMIT)
            .map_err(ConfigError::io)?;
        self.validate(&Config::parse(&old)?)?;
        if blake3::hash(&old).to_hex().as_str() != revision {
            return Err(ConfigError::field("$", "config_changed"));
        }
        match self.directory.open("config.previous.json", false) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(ConfigError::io(e)),
        }
        self.directory
            .atomic("config.previous.json", &old)
            .map_err(ConfigError::io)?;
        self.directory.verify_location().map_err(ConfigError::io)?;
        let lock_now = self
            .directory
            .open("config.lock", false)
            .map_err(ConfigError::io)?;
        let a = lock.metadata().map_err(ConfigError::io)?;
        let b = lock_now.metadata().map_err(ConfigError::io)?;
        if (a.dev(), a.ino()) != (b.dev(), b.ino())
            || self
                .directory
                .read("config.json", LIMIT)
                .map_err(ConfigError::io)?
                != old
        {
            return Err(ConfigError::field("$", "config_changed"));
        }
        if Instant::now() > deadline {
            return Err(ConfigError::field("$", "config_busy"));
        }
        self.directory
            .atomic("config.json", &encoded(config)?)
            .map_err(ConfigError::io)?;
        let saved = self.snapshot();
        if saved.saved.as_ref() != Some(config) || !saved.errors.is_empty() {
            return Err(ConfigError::field("$", "config_changed"));
        }
        Ok(saved)
    }
}
fn encoded(config: &Config) -> Result<Vec<u8>, ConfigError> {
    let mut bytes =
        serde_json::to_vec_pretty(config).map_err(|_| ConfigError::field("$", "invalid_config"))?;
    bytes.push(b'\n');
    if bytes.len() > LIMIT {
        return Err(ConfigError::field("$", "config_too_large"));
    }
    Ok(bytes)
}
fn lock(dir: &Directory) -> Result<std::fs::File, ConfigError> {
    let file = match dir.open("config.lock", true) {
        Ok(f) => {
            dir.sync().map_err(ConfigError::io)?;
            f
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            dir.open("config.lock", false).map_err(ConfigError::io)?
        }
        Err(e) => return Err(ConfigError::io(e)),
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0 {
        return Err(ConfigError::field("$", "config_busy"));
    }
    Ok(file)
}

enum Request {
    Read(oneshot::Sender<Settings>),
    Save {
        revision: String,
        config: Config,
        deadline: Instant,
        reply: oneshot::Sender<Result<Settings, ConfigError>>,
    },
    Wake,
}
#[derive(Clone)]
pub struct ConfigHandle {
    sender: mpsc::SyncSender<Request>,
    prepared: Arc<Prepared>,
}
impl ConfigHandle {
    /// Only management workers call this; every deletion unit must consult disk.
    pub(crate) fn history_policy(&self) -> Result<(Config, String), ConfigError> {
        self.prepared.read()
    }
    pub async fn read(&self) -> Result<Settings, ConfigError> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .try_send(Request::Read(tx))
            .map_err(|_| ConfigError::field("$", "config_busy"))?;
        tokio::time::timeout(Duration::from_secs(3), rx)
            .await
            .map_err(|_| ConfigError::field("$", "config_busy"))?
            .map_err(|_| ConfigError::field("$", "config_unavailable"))
    }
    pub async fn save(&self, revision: String, config: Config) -> Result<Settings, ConfigError> {
        let (reply, rx) = oneshot::channel();
        self.sender
            .try_send(Request::Save {
                revision,
                config,
                deadline: Instant::now() + Duration::from_secs(3),
                reply,
            })
            .map_err(|_| ConfigError::field("$", "config_busy"))?;
        tokio::time::timeout(Duration::from_secs(3), rx)
            .await
            .map_err(|_| ConfigError::field("$", "config_save_unconfirmed"))?
            .map_err(|_| ConfigError::field("$", "config_unavailable"))?
    }
}
pub struct ConfigService {
    handle: ConfigHandle,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ConfigService {
    pub fn start(prepared: Prepared) -> io::Result<Self> {
        let (sender, rx) = mpsc::sync_channel(8);
        let prepared = Arc::new(prepared);
        let handle = ConfigHandle {
            sender,
            prepared: prepared.clone(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = std::thread::Builder::new()
            .name("workbench-settings".into())
            .spawn(move || {
                // Reads/saves consult disk directly, and the cleanup scheduler
                // independently polls policy before every deletion. A watcher
                // had no consumer for its snapshots and delayed first requests
                // while the OS initialized it. Keep this worker ready immediately.
                while !stopping.load(Ordering::Acquire) {
                    match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(Request::Read(reply)) => {
                            let _ = reply.send(prepared.snapshot());
                        }
                        Ok(Request::Save {
                            revision,
                            config,
                            deadline,
                            reply,
                        }) => {
                            if !reply.is_closed() {
                                let _ = reply.send(prepared.save(&revision, &config, deadline));
                            }
                        }
                        Ok(Request::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok(Self {
            handle,
            stop,
            thread: Some(thread),
        })
    }
    pub fn handle(&self) -> ConfigHandle {
        self.handle.clone()
    }
}
impl Drop for ConfigService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.handle.sender.try_send(Request::Wake);
        if let Some(thread) = self.thread.take()
            && thread.is_finished()
        {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests;
