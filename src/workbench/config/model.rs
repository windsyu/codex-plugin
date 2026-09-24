use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::ConfigError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    #[serde(default)]
    pub launch: Launch,
    #[serde(default)]
    pub storage: Storage,
    #[serde(default)]
    pub history: History,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<Access>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: 2,
            launch: Launch::default(),
            storage: Storage::default(),
            history: History::default(),
            access: None,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Access {
    pub port: u16,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Launch {
    pub open_browser: bool,
    pub codex_bin: String,
    pub profile: Option<String>,
    pub provider_profile: String,
}
impl Default for Launch {
    fn default() -> Self {
        Self {
            open_browser: true,
            codex_bin: "codex".into(),
            profile: None,
            provider_profile: "unmanaged-custom".into(),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Storage {
    pub data_dir: Option<PathBuf>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct History {
    pub cleanup: Cleanup,
    pub library: crate::history::library::LibraryConfig,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cleanup {
    pub enabled: bool,
    pub retention: Retention,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Retention {
    pub enabled: bool,
    pub days: u32,
}
impl Default for Retention {
    fn default() -> Self {
        Self {
            enabled: false,
            days: 90,
        }
    }
}

impl Config {
    pub fn parse(bytes: &[u8]) -> Result<Self, ConfigError> {
        if bytes.len() > super::LIMIT {
            return Err(ConfigError::field("$", "config_too_large"));
        }
        // Typed serde rejects duplicate/unknown fields at every object level.
        // Report only a known field path and position, never the invalid value.
        let mut de = serde_json::Deserializer::from_slice(bytes);
        let config: Self = serde_path_to_error::deserialize(&mut de).map_err(|error| {
            let path = error.path().to_string();
            let safe = [
                "schemaVersion",
                "access",
                "access.port",
                "launch",
                "launch.openBrowser",
                "launch.codexBin",
                "launch.profile",
                "launch.providerProfile",
                "storage",
                "storage.dataDir",
                "history",
                "history.cleanup",
                "history.cleanup.enabled",
                "history.cleanup.retention",
                "history.cleanup.retention.enabled",
                "history.cleanup.retention.days",
            ];
            ConfigError {
                code: "invalid_config",
                field: if safe.contains(&path.as_str()) {
                    path
                } else {
                    "$".into()
                },
                line: Some(error.inner().line()),
                column: Some(error.inner().column()),
            }
        })?;
        de.end().map_err(|e| ConfigError {
            code: "invalid_config",
            field: "$".into(),
            line: Some(e.line()),
            column: Some(e.column()),
        })?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |field| ConfigError::field(field, "invalid_field");
        if !matches!(self.schema_version, 1 | 2) {
            return Err(ConfigError::field(
                "schemaVersion",
                "unsupported_config_version",
            ));
        }
        if self
            .access
            .as_ref()
            .is_some_and(|a| a.port != 0 && a.port < 1024)
        {
            return Err(invalid("access.port"));
        }
        let bin = &self.launch.codex_bin;
        if bin != "codex" && (!std::path::Path::new(bin).is_absolute() || bin.contains('\0')) {
            return Err(invalid("launch.codexBin"));
        }
        if self.launch.profile.as_ref().is_some_and(|p| {
            p.is_empty()
                || p.len() > 128
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        }) {
            return Err(invalid("launch.profile"));
        }
        if self.launch.provider_profile != "unmanaged-custom" {
            return Err(invalid("launch.providerProfile"));
        }
        if self
            .storage
            .data_dir
            .as_ref()
            .is_some_and(|p| !p.is_absolute() || p.as_os_str().as_encoded_bytes().contains(&0))
        {
            return Err(invalid("storage.dataDir"));
        }
        self.history
            .library
            .validate()
            .map_err(|code| ConfigError::field("history.library", code))?;
        let cleanup = &self.history.cleanup;
        if !(1..=3650).contains(&cleanup.retention.days) {
            return Err(invalid("history.cleanup.retention.days"));
        }
        if cleanup.retention.enabled && !cleanup.enabled {
            return Err(ConfigError::field(
                "history.cleanup.retention.enabled",
                "cleanup_disabled",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct Overrides {
    pub open_browser: Option<bool>,
    pub codex_bin: Option<PathBuf>,
    pub profile: Option<String>,
    pub provider_profile: Option<String>,
    pub data_dir: Option<PathBuf>,
}
impl Overrides {
    pub fn apply(&self, config: &Config) -> Config {
        let mut c = config.clone();
        if let Some(v) = self.open_browser {
            c.launch.open_browser = v;
        }
        if let Some(v) = &self.codex_bin {
            c.launch.codex_bin = v.to_string_lossy().into_owned();
        }
        if let Some(v) = &self.profile {
            c.launch.profile = Some(v.clone());
        }
        if let Some(v) = &self.provider_profile {
            c.launch.provider_profile = v.clone();
        }
        if let Some(v) = &self.data_dir {
            c.storage.data_dir = Some(v.clone());
        }
        c
    }
    pub fn fields(&self) -> Vec<&'static str> {
        [
            (self.open_browser.is_some(), "launch.openBrowser"),
            (self.codex_bin.is_some(), "launch.codexBin"),
            (self.profile.is_some(), "launch.profile"),
            (self.provider_profile.is_some(), "launch.providerProfile"),
            (self.data_dir.is_some(), "storage.dataDir"),
        ]
        .into_iter()
        .filter_map(|(present, field)| present.then_some(field))
        .collect()
    }
}
