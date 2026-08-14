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
    pub live_mode: String,
    pub scan_interval_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    pub max_raw_event_bytes: usize,
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
            live_mode: "off".into(),
            scan_interval_seconds: 30,
        }
    }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            max_raw_event_bytes: 64 * 1024 * 1024,
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
        self.storage.database = resolve_path(&base, &self.storage.database)?;
        self.storage.fingerprint_key_file =
            resolve_path(&base, &self.storage.fingerprint_key_file)?;
        self.server.bearer_token_file = resolve_path(&base, &self.server.bearer_token_file)?;
        for source in &mut self.sources {
            source.codex_home = resolve_path(&base, &source.codex_home)?;
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
        for source in &self.sources {
            if source.live_mode != "off" {
                bail!(
                    "V1 MVP only supports live_mode=off; got {}",
                    source.live_mode
                );
            }
            if source.scan_interval_seconds == 0 {
                bail!("scan_interval_seconds must be greater than zero");
            }
        }
        if self.capture.max_raw_event_bytes < 1024 {
            bail!("max_raw_event_bytes must be at least 1024");
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
