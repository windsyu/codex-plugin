//! User launchd jobs scoped by the same config/data identity as Application.
//! No shell, PID-based killing, credentials, or implicit project startup.
use super::{management, *};
use crate::workbench::{launch::read_entry, recording::fs::Directory};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::{
    fd::AsRawFd,
    unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
};
use std::process::Stdio;
use tokio::process::Command;

const SPEC: &str = "service.json";
const PLIST: &str = "service.plist";
const SPEC_LIMIT: usize = 64 * 1024;
const LAUNCHD_THROTTLE_SECONDS: u64 = 30;

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Specification {
    format: u32,
    label: String,
    executable: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    native_home: PathBuf,
    user_home: PathBuf,
    search_path: String,
    arguments: Vec<String>,
}
pub struct Manager {
    root: PathBuf,
    scope: String,
    config: PathBuf,
    data: PathBuf,
    user_home: PathBuf,
}
pub struct Guard(File);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}
impl Manager {
    /// Find an existing scope without parsing current launch settings. This is
    /// deliberately usable when a damaged config must not prevent `stop`.
    pub fn lookup(
        paths: &WorkbenchPaths,
        config_dir: Option<&Path>,
        data_dir: Option<&Path>,
        cwd: &Path,
    ) -> Result<Self> {
        let normalize = |path: &Path| super::super::config::location_for_application(path, cwd);
        let config = normalize(config_dir.unwrap_or(&paths.config_dir()))?;
        if let Some(data) = data_dir {
            return Self::new(paths, &config, &normalize(data)?);
        }
        let mut matches = Vec::new();
        if let Some(runtime) = runtime_directory(paths)? {
            for name in scope_names(&runtime)? {
                let d = runtime.dir(&name, false)?;
                let Some(bytes) = optional_read(&d, SPEC)? else {
                    continue;
                };
                let spec: Specification = serde_json::from_slice(&bytes)
                    .map_err(|_| anyhow::anyhow!("invalid private service registration; specify --data-dir to select a known scope"))?;
                if spec.config_dir != config {
                    continue;
                }
                let manager = Self::new(paths, &spec.config_dir, &spec.data_dir)?;
                ensure!(manager.scope == name, "service registration scope mismatch");
                manager.validate(&spec)?;
                matches.push(manager);
            }
        }
        ensure!(
            matches.len() <= 1,
            "multiple workbench data scopes use this configuration; specify --data-dir"
        );
        if let Some(manager) = matches.pop() {
            return Ok(manager);
        }
        // An ordinary foreground instance may not have a service manifest.
        // Only a valid, safely read config can contribute its data-directory
        // hint; malformed settings never block the explicit/default scope.
        let hint = match Directory::read_only(&config) {
            Ok(d) => match d.read("config.json", super::super::config::LIMIT) {
                Ok(bytes) => serde_json::from_slice::<super::super::config::Config>(&bytes)
                    .ok()
                    .filter(|c| c.validate().is_ok())
                    .and_then(|c| c.storage.data_dir),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let data = normalize(hint.as_deref().unwrap_or(&paths.history_dir()))?;
        Self::new(paths, &config, &data)
    }

    pub fn new(paths: &WorkbenchPaths, config: &Path, data: &Path) -> Result<Self> {
        let scope = blake3::hash(&serde_json::to_vec(&(config, data))?)
            .to_hex()
            .to_string();
        Ok(Self {
            root: paths.root.clone(),
            scope,
            config: config.into(),
            data: data.into(),
            user_home: paths
                .root
                .parent()
                .ok_or_else(|| anyhow::anyhow!("invalid workbench root"))?
                .into(),
        })
    }
    pub fn label(&self) -> String {
        format!("io.github.windsyu.code-view.{}", self.scope)
    }
    pub fn directory_path(&self) -> PathBuf {
        self.root.join("runtime").join(&self.scope)
    }
    fn directory(&self, create: bool) -> Result<Directory> {
        let root = if create {
            Directory::root(&self.root)?
        } else {
            Directory::read_only(&self.root)?
        };
        Ok(root.dir("runtime", create)?.dir(&self.scope, create)?)
    }
    pub fn lock(&self) -> Result<Guard> {
        let d = self.directory(true)?;
        lock_directory(&d)
    }
    /// Missing or unlocked instances are stopped; stale entry files are never
    /// enough to establish identity and are never deleted by this probe.
    pub fn running_entry(&self) -> Result<Option<BrowserEntry>> {
        let d = match self.directory(false) {
            Ok(d) => d,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let file = match d.open("instance.lock", false) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(None);
        }
        ensure!(
            std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock,
            "instance lock probe failed"
        );
        Ok(Some(read_entry(&d.path.join("instance.json"))?))
    }
    pub fn specification(
        &self,
        native_home: PathBuf,
        overrides: &Overrides,
    ) -> Result<Specification> {
        let mut arguments = vec![
            "--no-open".into(),
            "--config-dir".into(),
            text_path(&self.config)?,
            "--data-dir".into(),
            text_path(&self.data)?,
            "--managed-service".into(),
            self.label(),
        ];
        for (flag, value) in [
            (
                "--codex-bin",
                overrides
                    .codex_bin
                    .as_ref()
                    .map(|p| text_path(p))
                    .transpose()?,
            ),
            ("--profile", overrides.profile.clone()),
            ("--provider-profile", overrides.provider_profile.clone()),
        ] {
            if let Some(value) = value {
                arguments.extend([flag.to_owned(), value]);
            }
        }
        let search_path = std::env::join_paths(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .filter(|p| p.is_absolute()),
        )
        .map_err(|_| anyhow::anyhow!("invalid executable search path"))?;
        let spec = Specification {
            format: 1,
            label: self.label(),
            executable: std::env::current_exe()?.canonicalize()?,
            config_dir: self.config.clone(),
            data_dir: self.data.clone(),
            native_home,
            user_home: self.user_home.clone(),
            search_path: search_path.to_string_lossy().into_owned(),
            arguments,
        };
        self.validate(&spec)?;
        Ok(spec)
    }
    fn validate(&self, s: &Specification) -> Result<()> {
        ensure!(
            s.format == 1
                && s.label == self.label()
                && s.config_dir == self.config
                && s.data_dir == self.data
                && s.user_home == self.user_home
                && s.executable.is_absolute()
                && s.native_home.is_absolute(),
            "invalid service registration"
        );
        ensure!(
            s.arguments.len() <= 20
                && s.arguments
                    .iter()
                    .all(|v| v.len() <= 4096 && !v.chars().any(char::is_control))
                && s.search_path.len() <= 16384
                && !s.search_path.chars().any(char::is_control),
            "invalid service launch arguments"
        );
        for path in [
            &s.executable,
            &s.config_dir,
            &s.data_dir,
            &s.native_home,
            &s.user_home,
        ] {
            text_path(path)?;
        }
        let required = [
            "--no-open".to_owned(),
            "--config-dir".into(),
            text_path(&self.config)?,
            "--data-dir".into(),
            text_path(&self.data)?,
            "--managed-service".into(),
            self.label(),
        ];
        ensure!(
            s.arguments.starts_with(&required),
            "invalid service launch identity"
        );
        let mut flags = std::collections::HashSet::new();
        for pair in s.arguments[required.len()..].chunks(2) {
            ensure!(
                pair.len() == 2
                    && matches!(
                        pair[0].as_str(),
                        "--codex-bin" | "--profile" | "--provider-profile"
                    )
                    && flags.insert(pair[0].as_str()),
                "invalid service launch defaults"
            );
        }
        ensure!(
            serde_json::to_vec(s)?.len() <= SPEC_LIMIT,
            "service registration exceeds size limit"
        );
        Ok(())
    }
    fn saved(&self) -> Result<Option<Specification>> {
        let d = match self.directory(false) {
            Ok(d) => d,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        match d.read(SPEC, SPEC_LIMIT) {
            Ok(bytes) => {
                let s = serde_json::from_slice(&bytes)
                    .map_err(|_| anyhow::anyhow!("invalid private service registration"))?;
                self.validate(&s)?;
                Ok(Some(s))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn autostart_path(&self) -> PathBuf {
        self.user_home
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", self.label()))
    }
    pub fn autostart_enabled(&self) -> Result<bool> {
        let Some(s) = self.saved()? else {
            ensure!(
                !self.autostart_path().try_exists()?,
                "unknown login service registration"
            );
            return Ok(false);
        };
        self.check_login_plist(&s)
    }
    fn check_login_plist(&self, s: &Specification) -> Result<bool> {
        let path = self.autostart_path();
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
            Ok(_) => {
                check_login_parent(&self.user_home, false)?;
                let bytes = read_owned(&path)?;
                ensure!(
                    bytes == self.plist(s)?.as_bytes(),
                    "login service file was changed; preserve it and resolve the conflict"
                );
                Ok(true)
            }
        }
    }
    pub async fn loaded(&self) -> Result<bool> {
        launchctl(&["print".into(), self.target()]).await
    }
    fn target(&self) -> String {
        format!("gui/{}/{}", unsafe { libc::geteuid() }, self.label())
    }
    pub async fn start(&self, spec: Specification) -> Result<(BrowserEntry, management::Status)> {
        require_macos()?;
        let _guard = self.lock()?;
        installation_available(&spec.executable)?;
        self.validate(&spec)?;
        let saved = self.saved()?;
        if let Some(entry) = self.running_entry()? {
            let status = management::control(&entry, management::Action::Status).await?;
            ensure!(
                status.service_label.as_deref() == Some(&self.label()),
                "a foreground instance is running; use code-view stop before starting the background service"
            );
            ensure!(
                !status.stopping,
                "workbench is stopping; wait before starting it again"
            );
            let saved = saved
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("service registration missing"))?;
            compatible_saved(saved, &spec)?;
            return Ok((entry, status));
        }
        if self.loaded().await? {
            let saved = saved
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("loaded service registration missing"))?;
            compatible_saved(saved, &spec)?;
            self.check_runtime_plist(saved)?;
            ensure!(
                launchctl(&["kickstart".into(), self.target()]).await?,
                "cannot start the existing user service"
            );
        } else {
            let auto = self.autostart_enabled()?;
            let spec = self.inactive_specification(spec, saved.as_ref(), auto)?;
            let d = self.directory(true)?;
            if let Some(saved) = &saved {
                self.check_runtime_plist(saved)?;
            } else {
                ensure!(
                    optional_read(&d, PLIST)?.is_none(),
                    "runtime service file has no verified registration; preserve it before retrying"
                );
            }
            d.atomic(SPEC, &serde_json::to_vec(&spec)?)?;
            d.atomic(PLIST, self.plist(&spec)?.as_bytes())?;
            self.ensure_log()?;
            ensure!(
                launchctl(&["enable".into(), self.target()]).await?,
                "cannot enable the user service"
            );
            ensure!(
                launchctl(&[
                    "bootstrap".into(),
                    format!("gui/{}", unsafe { libc::geteuid() }),
                    text_path(&d.path.join(PLIST))?
                ])
                .await?,
                "cannot register the background service in this logged-in macOS session"
            );
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(entry)) = self.running_entry()
                && let Ok(status) = management::control(&entry, management::Action::Status).await
                && status.service_label.as_deref() == Some(&self.label())
                && !status.stopping
            {
                return Ok((entry, status));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!(
            "service readiness was not confirmed; use code-view status and inspect the private service.log (the job may still be starting)"
        )
    }
    pub async fn stop(&self) -> Result<()> {
        let _guard = self.lock()?;
        let entry = self.running_entry()?;
        let loaded = if cfg!(target_os = "macos") {
            self.loaded().await?
        } else {
            false
        };
        if let Some(entry) = entry {
            let status = management::control(&entry, management::Action::Status).await?;
            if let Some(label) = status.service_label {
                ensure!(
                    label == self.label() && self.saved()?.is_some(),
                    "service identity mismatch"
                );
                ensure!(
                    loaded && launchctl(&["bootout".into(), self.target()]).await?,
                    "could not unload the managed service"
                );
            } else {
                management::control(&entry, management::Action::Shutdown).await?;
            }
        } else if loaded {
            ensure!(self.saved()?.is_some(), "unknown service registration");
            ensure!(
                launchctl(&["bootout".into(), self.target()]).await?,
                "could not unload the managed service"
            );
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
        while tokio::time::Instant::now() < deadline {
            if self.running_entry()?.is_none()
                && (!cfg!(target_os = "macos") || !self.loaded().await?)
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("complete shutdown was not confirmed; do not delete the instance lock")
    }
    pub fn set_autostart(&self, enabled: bool, fallback: Specification) -> Result<()> {
        require_macos()?;
        let _guard = self.lock()?;
        let spec = self.autostart_specification(enabled, fallback)?;
        let present = self.check_login_plist(&spec)?;
        if enabled {
            self.ensure_log()?;
            self.directory(true)?
                .atomic(SPEC, &serde_json::to_vec(&spec)?)?;
            self.write_login_plist(&spec)?;
        } else if present {
            fs::remove_file(self.autostart_path())?;
        }
        Ok(())
    }
    fn autostart_specification(
        &self,
        enabled: bool,
        fallback: Specification,
    ) -> Result<Specification> {
        let saved = self.saved()?;
        if enabled {
            installation_available(&fallback.executable)?;
            if let Some(saved) = &saved {
                compatible_saved(saved, &fallback)?;
            }
        }
        let spec = saved.unwrap_or(fallback);
        if enabled {
            installation_available(&spec.executable)?;
        }
        Ok(spec)
    }
    fn inactive_specification(
        &self,
        mut requested: Specification,
        saved: Option<&Specification>,
        autostart: bool,
    ) -> Result<Specification> {
        if let Some(saved) = saved {
            for flag in ["--codex-bin", "--profile", "--provider-profile"] {
                if argument(&requested, flag).is_none()
                    && let Some(value) = argument(saved, flag)
                {
                    requested.arguments.extend([flag.into(), value.into()]);
                }
            }
            requested.search_path = saved.search_path.clone();
            // Stable argument order makes semantically identical defaults
            // compare equal even when only one override was supplied again.
            let mut arguments = requested.arguments[..7].to_vec();
            for flag in ["--codex-bin", "--profile", "--provider-profile"] {
                if let Some(value) = argument(&requested, flag) {
                    arguments.extend([flag.into(), value.into()]);
                }
            }
            requested.arguments = arguments;
            ensure!(
                !autostart || requested == *saved,
                "login startup is enabled; run code-view autostart off, start with the new defaults, then code-view autostart on"
            );
        }
        self.validate(&requested)?;
        Ok(requested)
    }
    fn check_runtime_plist(&self, spec: &Specification) -> Result<bool> {
        let d = self.directory(false)?;
        if let Some(actual) = optional_read(&d, PLIST)? {
            ensure!(
                actual == self.plist(spec)?.as_bytes(),
                "runtime service file was changed; preserve it and resolve the conflict"
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }
    fn ensure_log(&self) -> Result<()> {
        let d = self.directory(true)?;
        match d.open("service.log", true) {
            Ok(file) => {
                file.sync_all()?;
                d.sync()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if d.open("service.log", false)?.metadata()?.len() > 1024 * 1024 {
                    d.atomic("service.log", b"")?;
                }
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
    fn write_login_plist(&self, spec: &Specification) -> Result<()> {
        let parent = check_login_parent(&self.user_home, true)?;
        let path = self.autostart_path();
        if path.try_exists()? {
            read_owned(&path)?;
        }
        let temporary = parent.join(format!(".code-view-{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<()> {
            use std::io::Write;
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temporary)?;
            f.write_all(self.plist(spec)?.as_bytes())?;
            f.sync_all()?;
            check_login_parent(&self.user_home, false)?;
            fs::rename(&temporary, &path)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
    fn plist(&self, s: &Specification) -> Result<String> {
        let strings = std::iter::once(text_path(&s.executable)?)
            .chain(s.arguments.iter().cloned())
            .map(|v| format!("<string>{}</string>", xml(&v)))
            .collect::<String>();
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{}</array><key>WorkingDirectory</key><string>{}</string><key>EnvironmentVariables</key><dict><key>HOME</key><string>{}</string><key>CODEX_HOME</key><string>{}</string><key>PATH</key><string>{}</string></dict><key>Umask</key><integer>63</integer><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>30</integer><key>ExitTimeOut</key><integer>30</integer><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string></dict></plist>\n",
            xml(&s.label),
            strings,
            xml(&text_path(&s.user_home)?),
            xml(&text_path(&s.user_home)?),
            xml(&text_path(&s.native_home)?),
            xml(&s.search_path),
            xml(&text_path(&self.directory_path().join("service.log"))?),
            xml(&text_path(&self.directory_path().join("service.log"))?)
        ))
    }
    pub fn verify_invocation(&self, label: &str) -> Result<()> {
        let spec = self
            .saved()?
            .ok_or_else(|| anyhow::anyhow!("service registration missing"))?;
        ensure!(
            spec.label == label && spec.executable == std::env::current_exe()?.canonicalize()?,
            "managed service invocation does not match its private registration"
        );
        // start holds service.lock through readiness. A child must not acquire
        // it and deadlock its parent. Installer preflight takes that lock and
        // also rejects loaded launchd jobs, including login startup.
        installation_available(&spec.executable)?;
        Ok(())
    }
}
fn xml(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn text_path(p: &Path) -> Result<String> {
    let value = p
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("service path must be UTF-8"))?;
    ensure!(
        value.len() <= 4096 && !value.chars().any(char::is_control),
        "invalid service path"
    );
    Ok(value.into())
}
fn require_macos() -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "background services currently support macOS only"
    );
    Ok(())
}
fn argument<'a>(spec: &'a Specification, flag: &str) -> Option<&'a str> {
    spec.arguments[7..]
        .chunks(2)
        .find_map(|pair| (pair.len() == 2 && pair[0] == flag).then(|| pair[1].as_str()))
}
fn lock_directory(directory: &Directory) -> Result<Guard> {
    let file = match directory.open("service.lock", true) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            directory.open("service.lock", false)?
        }
        Err(e) => return Err(e.into()),
    };
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another service management operation is in progress; retry"
    );
    Ok(Guard(file))
}
fn compatible_saved(saved: &Specification, requested: &Specification) -> Result<()> {
    ensure!(
        saved.executable == requested.executable && saved.native_home == requested.native_home,
        "registered executable or native home differs; stop the service and disable login startup before changing it"
    );
    for flag in ["--codex-bin", "--profile", "--provider-profile"] {
        if let Some(value) = argument(requested, flag) {
            ensure!(
                argument(saved, flag) == Some(value),
                "explicit service launch defaults differ; stop the service before changing them"
            );
        }
    }
    Ok(())
}
fn installation_available(executable: &Path) -> Result<()> {
    let Some(bin) = executable
        .parent()
        .filter(|p| p.file_name() == Some(std::ffi::OsStr::new("bin")))
    else {
        return Ok(());
    };
    let Some(prefix) = bin.parent() else {
        return Ok(());
    };
    match fs::symlink_metadata(prefix.join(".code-view-install-lock")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
        Ok(_) => anyhow::bail!("installation management is in progress; retry after it completes"),
    }
}
fn optional_read(directory: &Directory, name: &str) -> Result<Option<Vec<u8>>> {
    match directory.read(name, SPEC_LIMIT) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn runtime_directory(paths: &WorkbenchPaths) -> Result<Option<Directory>> {
    match Directory::read_only(&paths.root).and_then(|root| root.dir("runtime", false)) {
        Ok(runtime) => Ok(Some(runtime)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn scope_names(runtime: &Directory) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for (count, name) in runtime.entries()?.enumerate() {
        ensure!(
            count < 256,
            "too many service registrations to inspect safely"
        );
        let name = name?;
        if name.len() == 64 && name.bytes().all(|v| v.is_ascii_hexdigit()) {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}
async fn launchctl(args: &[String]) -> Result<bool> {
    require_macos()?;
    let mut child = Command::new("/bin/launchctl")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    Ok(tokio::time::timeout(launchctl_timeout(args), child.wait())
        .await
        .map_err(|_| anyhow::anyhow!("launchctl timed out"))??
        .success())
}
fn launchctl_timeout(args: &[String]) -> Duration {
    // A recently exited job may still be within launchd's spawn throttle.
    // kickstart waits for that launch; killing its client at eight seconds
    // reports failure even though launchd can still complete the request.
    // bootout also waits while launchd grants the job its 30-second exit
    // budget; preserve the caller until that cleanup can finish.
    if matches!(
        args.first().map(String::as_str),
        Some("kickstart" | "bootout")
    ) {
        Duration::from_secs(LAUNCHD_THROTTLE_SECONDS + 10)
    } else {
        Duration::from_secs(8)
    }
}
fn read_owned(path: &Path) -> Result<Vec<u8>> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let m = f.metadata()?;
    ensure!(
        m.is_file()
            && m.uid() == unsafe { libc::geteuid() }
            && m.mode() & 0o077 == 0
            && m.nlink() == 1
            && m.len() <= SPEC_LIMIT as u64,
        "unsafe login service file"
    );
    let mut b = Vec::new();
    f.take(SPEC_LIMIT as u64 + 1).read_to_end(&mut b)?;
    ensure!(b.len() <= SPEC_LIMIT, "login service file too large");
    Ok(b)
}
fn check_login_parent(home: &Path, create: bool) -> Result<PathBuf> {
    let mut path = home.to_path_buf();
    for name in ["Library", "LaunchAgents"] {
        path.push(name);
        if create {
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        let m = fs::symlink_metadata(&path)?;
        ensure!(
            m.is_dir()
                && !m.file_type().is_symlink()
                && m.uid() == unsafe { libc::geteuid() }
                && m.mode() & 0o022 == 0,
            "unsafe login service directory"
        );
    }
    Ok(path)
}

/// Installer hook: inspect all scopes, never stop a process or touch another
/// executable's registration. Unknown/modified owned files fail closed.
pub async fn installation_guard(paths: &WorkbenchPaths, remove: bool) -> Result<()> {
    let Some(runtime) = runtime_directory(paths)? else {
        return Ok(());
    };
    let executable = std::env::current_exe()?.canonicalize()?;
    struct Checked {
        manager: Manager,
        directory: Directory,
        autostart: bool,
        runtime_plist: bool,
    }
    let mut checked = Vec::new();
    let mut guards = Vec::new();
    // Preflight every owned scope before deleting any registration. Holding
    // all management locks through commit prevents concurrent start/autostart.
    for name in scope_names(&runtime)? {
        let d = runtime.dir(&name, false)?;
        // A first start creates/locks the scope before checking the installer
        // reservation and may not have written SPEC yet. Lock before even the
        // absence check, otherwise installation could overtake that start.
        guards.push(lock_directory(&d)?);
        let Some(bytes) = optional_read(&d, SPEC)? else {
            continue;
        };
        let spec: Specification = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid service registration; installation preserved"))?;
        if spec.executable != executable {
            continue;
        }
        let manager = Manager::new(paths, &spec.config_dir, &spec.data_dir)?;
        ensure!(manager.scope == name, "service registration scope mismatch");
        manager.validate(&spec)?;
        ensure!(
            manager.running_entry()?.is_none()
                && (!cfg!(target_os = "macos") || !manager.loaded().await?),
            "active workbench service; run code-view stop with its config/data scope before upgrading or uninstalling"
        );
        let auto = manager.check_login_plist(&spec)?;
        let runtime_plist = manager.check_runtime_plist(&spec)?;
        checked.push(Checked {
            manager,
            directory: d,
            autostart: auto,
            runtime_plist,
        });
    }
    if remove {
        for checked in &checked {
            if checked.autostart {
                fs::remove_file(checked.manager.autostart_path())?;
                File::open(check_login_parent(&checked.manager.user_home, false)?)?.sync_all()?;
            }
            if checked.runtime_plist {
                checked.directory.remove(PLIST)?;
            }
            checked.directory.remove(SPEC)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
