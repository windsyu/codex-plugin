use std::collections::HashSet;
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub sources: Vec<SourceConfig>,
    pub capture: CaptureConfig,
    #[serde(skip)]
    pub config_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub bearer_token_file: PathBuf,
    pub strict_origin: bool,
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub database: PathBuf,
    pub blob_dir: PathBuf,
    pub fingerprint_key_file: PathBuf,
    pub raw_event_retention_days: u64,
    pub delta_retention_days: u64,
    pub blob_retention_days: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceConfig {
    pub name: String,
    pub codex_home: PathBuf,
    pub app_server_socket: Option<PathBuf>,
    pub live_mode: String,
    pub scan_interval_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    pub max_raw_event_bytes: usize,
    pub inline_blob_bytes: usize,
    pub keep_reasoning: bool,
    pub keep_raw_json: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            storage: StorageConfig::default(),
            sources: vec![SourceConfig::default()],
            capture: CaptureConfig::default(),
            config_dir: PathBuf::from("."),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:4765".parse().expect("valid default bind"),
            bearer_token_file: PathBuf::from("observer-data/token"),
            strict_origin: true,
            allowed_origins: vec!["http://127.0.0.1:4765".into()],
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            database: PathBuf::from("observer-data/observer.sqlite"),
            blob_dir: PathBuf::from("observer-data/blobs"),
            fingerprint_key_file: PathBuf::from("observer-data/fingerprint.key"),
            raw_event_retention_days: 30,
            delta_retention_days: 7,
            blob_retention_days: 14,
        }
    }
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            name: "default".into(),
            codex_home: PathBuf::from("~/.codex"),
            app_server_socket: None,
            live_mode: "off".into(),
            scan_interval_seconds: 30,
        }
    }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            max_raw_event_bytes: 64 * 1024 * 1024,
            inline_blob_bytes: 256 * 1024,
            keep_reasoning: true,
            keep_raw_json: true,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let config_dir = absolute.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut config = if absolute.exists() {
            let text = fs::read_to_string(&absolute)
                .with_context(|| format!("read config {}", absolute.display()))?;
            toml::from_str::<Config>(&text)
                .with_context(|| format!("parse config {}", absolute.display()))?
        } else {
            Config::default()
        };
        config.config_dir = config_dir;
        config.resolve_paths()?;
        Ok(config)
    }

    fn resolve_paths(&mut self) -> Result<()> {
        let base = self.config_dir.clone();
        self.storage.database =
            canonicalize_allow_missing(&resolve_path(&base, &self.storage.database)?)?;
        self.storage.blob_dir =
            canonicalize_allow_missing(&resolve_path(&base, &self.storage.blob_dir)?)?;
        self.storage.fingerprint_key_file =
            canonicalize_allow_missing(&resolve_path(&base, &self.storage.fingerprint_key_file)?)?;
        self.server.bearer_token_file =
            canonicalize_allow_missing(&resolve_path(&base, &self.server.bearer_token_file)?)?;
        for source in &mut self.sources {
            let resolved_home = resolve_path(&base, &source.codex_home)?;
            source.codex_home = canonicalize_allow_missing(&resolved_home)?;
            if let Some(socket) = &source.app_server_socket {
                let resolved = resolve_path(&base, socket)?;
                source.app_server_socket = Some(canonicalize_parent(&resolved)?);
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if !matches!(self.server.bind.ip(), IpAddr::V4(ip) if ip.is_loopback())
            && !matches!(self.server.bind.ip(), IpAddr::V6(ip) if ip.is_loopback())
        {
            bail!("V1 only permits a loopback server bind");
        }
        if self.sources.is_empty() {
            bail!("at least one source is required");
        }
        let mut source_homes = HashSet::new();
        for source in &self.sources {
            if !source_homes.insert(&source.codex_home) {
                bail!(
                    "duplicate source codex_home {}",
                    source.codex_home.display()
                );
            }
            if !matches!(
                source.live_mode.as_str(),
                "off" | "observe_new" | "attach_loaded"
            ) {
                bail!(
                    "live_mode must be off, observe_new, or attach_loaded; got {}",
                    source.live_mode
                );
            }
            if source.live_mode != "off" && source.app_server_socket.is_none() {
                bail!(
                    "source {} enables live mode without app_server_socket",
                    source.name
                );
            }
            if source.scan_interval_seconds == 0 {
                bail!("scan_interval_seconds must be greater than zero");
            }
            for (label, path) in [
                ("database", &self.storage.database),
                ("blob_dir", &self.storage.blob_dir),
                ("fingerprint_key_file", &self.storage.fingerprint_key_file),
                ("bearer_token_file", &self.server.bearer_token_file),
            ] {
                if path.starts_with(&source.codex_home) {
                    bail!("{label} must not be located inside a Codex source");
                }
            }
        }
        if self.storage.database.starts_with(&self.storage.blob_dir) {
            bail!("database must not be located inside blob_dir");
        }
        if self.capture.max_raw_event_bytes < 1024 {
            bail!("max_raw_event_bytes must be at least 1024");
        }
        if self.capture.inline_blob_bytes < 1024
            || self.capture.inline_blob_bytes > self.capture.max_raw_event_bytes
        {
            bail!("inline_blob_bytes must be between 1024 and max_raw_event_bytes");
        }
        Ok(())
    }

    pub fn database_path(&self) -> &Path {
        &self.storage.database
    }

    pub fn minimum_scan_interval(&self) -> u64 {
        self.sources
            .iter()
            .map(|s| s.scan_interval_seconds)
            .min()
            .unwrap_or(30)
    }
}

fn canonicalize_parent(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().context("configured path has no parent")?;
    let name = path
        .file_name()
        .context("configured path has no file name")?;
    Ok(if parent.exists() {
        fs::canonicalize(parent)?.join(name)
    } else {
        path.to_path_buf()
    })
}

fn canonicalize_allow_missing(path: &Path) -> Result<PathBuf> {
    let path = normalize_lexical(path);
    let path = path.as_path();
    if path.exists() {
        return fs::canonicalize(path).with_context(|| format!("canonicalize {}", path.display()));
    }
    let mut cursor = path;
    let mut missing = Vec::new();
    while !cursor.exists() {
        let name = cursor
            .file_name()
            .context("configured path has no existing ancestor")?;
        missing.push(name.to_os_string());
        cursor = cursor
            .parent()
            .context("configured path has no existing ancestor")?;
    }
    let mut normalized = fs::canonicalize(cursor)?;
    for component in missing.iter().rev() {
        normalized.push(component);
    }
    Ok(normalized)
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn resolve_path(base: &Path, value: &Path) -> Result<PathBuf> {
    let expanded = if let Some(text) = value.to_str() {
        if text == "~" || text.starts_with("~/") {
            let home = std::env::var_os("HOME").context("HOME is not set")?;
            PathBuf::from(home).join(text.trim_start_matches("~/"))
        } else {
            value.to_path_buf()
        }
    } else {
        value.to_path_buf()
    };
    Ok(if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn canonicalizes_existing_socket_parent() -> Result<()> {
        let temp = TempDir::new()?;
        let nested = temp.path().join("nested");
        fs::create_dir_all(&nested)?;
        let represented = nested.join("..").join("nested").join("control.sock");
        let normalized = canonicalize_parent(&represented)?;
        assert_eq!(normalized, fs::canonicalize(&nested)?.join("control.sock"));
        assert!(!normalized.to_string_lossy().contains(".."));
        Ok(())
    }

    #[test]
    fn canonicalizes_missing_paths_through_existing_ancestor() -> Result<()> {
        let temp = TempDir::new()?;
        let nested = temp.path().join("a/../b/database.sqlite");
        let normalized = canonicalize_allow_missing(&nested)?;
        assert_eq!(
            normalized,
            fs::canonicalize(temp.path())?.join("b/database.sqlite")
        );
        assert!(!normalized.to_string_lossy().contains(".."));
        Ok(())
    }

    #[test]
    fn rejects_duplicate_sources_and_observer_writes_inside_codex_home() -> Result<()> {
        let temp = TempDir::new()?;
        let codex_home = temp.path().join("codex");
        let mut config = Config::default();
        config.sources[0].codex_home = codex_home.clone();
        config.sources.push(config.sources[0].clone());
        config.storage.database = temp.path().join("observer.sqlite");
        config.storage.blob_dir = temp.path().join("blobs");
        config.storage.fingerprint_key_file = temp.path().join("fingerprint.key");
        config.server.bearer_token_file = temp.path().join("token");
        assert!(config.validate().is_err());

        config.sources.truncate(1);
        config.storage.blob_dir = codex_home.join("observer-blobs");
        assert!(config.validate().is_err());
        Ok(())
    }
}
