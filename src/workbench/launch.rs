//! Production entry into the native workbench; owns only this run's resources.
use anyhow::{Context, Result, ensure};
use portable_pty::CommandBuilder;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

use super::{
    capture,
    decode::DecoderLimits,
    live::{LiveHub, LiveLimits},
    observe::Observer,
    paths::WorkbenchPaths,
    proxy::ProxyServer,
    recording::{Recorder, RecorderOptions},
    rollout::RolloutReader,
    terminal::TerminalHost,
    web::ReadingServer,
};
mod entry;
mod profile;
pub use entry::{BrowserEntry, read_entry};

// Evidence for diagnostics, never a startup allowlist. Different releases can
// use the same native flags and model protocol; the actual route checks remain.
const CHECKED_CLI_VERSIONS: &[&str] = &["0.154.0", "0.155.1"];

pub struct LaunchOptions {
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub workbench_paths: WorkbenchPaths,
    pub executable: PathBuf,
    pub native_profile: Option<String>,
    pub resume: Option<uuid::Uuid>,
    pub entry_file: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}

pub fn native_home() -> Result<PathBuf> {
    match std::env::var_os("CODEX_HOME") {
        Some(home) => Ok(PathBuf::from(home)),
        None => Ok(super::paths::user_home()?.join(".codex")),
    }
}
fn executable(path: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let candidates: Vec<_> = if path.components().count() > 1 || path.is_absolute() {
        vec![path.to_path_buf()]
    } else {
        std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths)
                    .map(|p| p.join(path))
                    .collect()
            })
            .unwrap_or_default()
    };
    candidates
        .into_iter()
        .find_map(|candidate| {
            let path = candidate.canonicalize().ok()?;
            let meta = path.metadata().ok()?;
            (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(path)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "official Codex executable was not found; install Codex CLI or use --codex-bin"
            )
        })
}
async fn version(path: &Path, home: &Path) -> Result<String> {
    let mut child = tokio::process::Command::new(path)
        .arg("--version")
        .env("CODEX_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| anyhow::anyhow!("could not check the installed CLI version"))?;
    let mut stdout = child.stdout.take().unwrap().take(4097);
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        ensure!(bytes.len() <= 4096, "CLI version output exceeds bound");
        let status = child.wait().await?;
        ensure!(status.success(), "installed CLI version command failed");
        // Accept release/prerelease/build identifiers without echoing arbitrary
        // subprocess output into logs. This is an identity/shape check, not a
        // minimum or exact-version compatibility claim.
        let release = bytes
            .trim_ascii()
            .strip_prefix(b"codex-cli ")
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 64
                    && s[0].is_ascii_digit()
                    && s.iter()
                        .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(b))
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "installed executable did not report a recognizable Codex CLI version"
                )
            })?;
        Ok::<_, anyhow::Error>(String::from_utf8(release.to_vec()).expect("ASCII CLI version"))
    })
    .await
    .context("CLI version check timed out")?
}

pub struct WorkbenchRun {
    // Field order deliberately stops the native process before removing proxy,
    // listeners, observers or pairing credentials. CLI is never respawned.
    terminal: TerminalHost,
    pub web: ReadingServer,
    proxy: ProxyServer,
    _observer: Observer,
    _users: RolloutReader,
    _recorder: Recorder,
    entry: entry::EntryFile,
    pub epoch: uuid::Uuid,
    pub cli_version: String,
    _settings: Option<super::config::ConfigService>,
}
impl WorkbenchRun {
    pub async fn start(options: LaunchOptions) -> Result<Self> {
        Self::start_inner(options, None).await
    }
    pub async fn start_configured(
        options: LaunchOptions,
        config: super::config::Prepared,
    ) -> Result<Self> {
        Self::start_inner(options, Some(config)).await
    }
    async fn start_inner(
        options: LaunchOptions,
        config: Option<super::config::Prepared>,
    ) -> Result<Self> {
        let settings = config
            .map(super::config::ConfigService::start)
            .transpose()?;
        let cwd = options
            .cwd
            .canonicalize()
            .map_err(|_| anyhow::anyhow!("project directory is unavailable"))?;
        ensure!(
            cwd.is_dir() && cwd.ancestors().count() <= 64,
            "project root must be a supported directory"
        );
        let home = options
            .home
            .canonicalize()
            .map_err(|_| anyhow::anyhow!("native Codex home must already exist"))?;
        ensure!(home.is_dir(), "native Codex home must be a directory");
        if let Some(id) = options.resume {
            ensure!(!id.is_nil(), "resume requires an explicit native thread ID");
        }
        let binary = executable(&options.executable)?;
        let cli_version = version(&binary, &home).await?;
        if !CHECKED_CLI_VERSIONS.contains(&cli_version.as_str()) {
            eprintln!("codex-view: CLI {cli_version} 尚未完成本工作台的版本回归，继续启动。");
        }
        let profile = profile::Profile::load(&home, &cwd, options.native_profile.as_deref())?;
        let hub = LiveHub::new(LiveLimits::default());
        let epoch = hub.epoch();
        let data_dir = options
            .data_dir
            .unwrap_or_else(|| options.workbench_paths.history_dir());
        let default_history = data_dir == options.workbench_paths.history_dir();
        let project_name = profile
            .policy
            .scrub(
                cwd.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("当前项目"),
            )
            .as_str()
            .to_owned();
        let mut recording = RecorderOptions::new(data_dir, &cwd, project_name);
        if default_history {
            recording.private_parent = Some(options.workbench_paths.root);
        }
        let recorder = Recorder::start(&hub, recording)?;
        let web_listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .context("reserve workbench loopback listener")?;
        let (capture, receiver) = capture::channel(8 * 1024 * 1024, 256);
        let proxy = ProxyServer::bind(profile.upstream.clone(), capture).await?;
        let observer = Observer::start(
            receiver,
            hub.clone(),
            profile.policy.clone(),
            DecoderLimits::default(),
        )?;
        let users = RolloutReader::start(&home, hub.clone(), profile.policy.clone())?;
        let mut command = CommandBuilder::new(binary);
        command.cwd(&cwd);
        command.env("CODEX_HOME", &home);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        // This child renders into a color xterm, not the launcher's log sink.
        command.env_remove("NO_COLOR");
        if let Some(name) = &options.native_profile {
            command.args(["--profile", name]);
        }
        command.arg("-c");
        command.arg(format!(
            "model_providers.{}.base_url={}",
            profile.provider,
            toml::Value::String(proxy.child_base_url())
        ));
        command.args(["-c", "tui.animations=false"]);
        if let Some(id) = options.resume {
            command.args(["resume".to_owned(), id.to_string()]);
        }
        profile.unchanged()?;
        let terminal = TerminalHost::spawn(epoch, command, 45, 120).map_err(|_| {
            anyhow::anyhow!("native CLI startup failed; command details suppressed")
        })?;
        let web = ReadingServer::bind_configured(
            hub,
            terminal.handle(),
            web_listener,
            settings.as_ref().map(|s| s.handle()),
            Some(super::workspace::Handle::start(&cwd).context("open workspace reader")?),
        )
        .await?;
        let entry = entry::EntryFile::create(
            options.entry_file.as_deref(),
            &BrowserEntry {
                format: "codex-view-entry-v1".into(),
                run_epoch: epoch,
                address: format!("http://{}", web.address()),
                url: web.bootstrap_url(),
                cli_pid: terminal.process_id(),
            },
        )
        .context("create private browser entry")?;
        Ok(Self {
            terminal,
            web,
            proxy,
            _observer: observer,
            _users: users,
            _recorder: recorder,
            entry,
            epoch,
            cli_version,
            _settings: settings,
        })
    }
    pub fn process_id(&self) -> u32 {
        self.terminal.process_id()
    }
    pub fn entry_file(&self) -> &Path {
        &self.entry.path
    }
    pub fn healthy(&self) -> bool {
        !self.proxy.is_finished() && !self.web.is_finished() && !self.terminal.is_finished()
    }
}

pub async fn open_browser(url: &str) -> Result<()> {
    // The capability is passed directly to the OS browser launcher, never a log
    // or a shell command string. Other platforms stay outside the tested matrix.
    ensure!(
        cfg!(target_os = "macos"),
        "automatic browser opening is not validated on this host"
    );
    let mut command = tokio::process::Command::new("/usr/bin/open");
    let mut child = command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .context("browser opener timed out")??;
    ensure!(
        status.success(),
        "browser could not be opened; the private pairing entry remains available"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
