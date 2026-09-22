//! Version-scoped route adapter, not an implementation of Codex config merging.
//! Only the tested base/named-user profile route fields are selected here; the
//! original files, project settings, permissions and credentials stay with CLI.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::super::{config_sources, proxy::Upstream, redaction::RedactionPolicy};
use anyhow::{Result, bail, ensure};

const CONFIG_BYTES: u64 = 2 * 1024 * 1024;

pub(super) struct Profile {
    pub provider: String,
    pub upstream: Upstream,
    pub policy: Arc<RedactionPolicy>,
    sources: Vec<(PathBuf, Vec<u8>)>,
}

pub(super) fn identifier(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
        "unsupported profile identifier"
    );
    Ok(())
}
fn read(path: &Path) -> Result<(toml::Value, Vec<u8>)> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| anyhow::anyhow!("cannot open native configuration (details suppressed)"))?;
    ensure!(
        file.metadata()?.is_file(),
        "native configuration must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("cannot read native configuration"))?;
    ensure!(
        bytes.len() as u64 <= CONFIG_BYTES,
        "native configuration exceeds inspection bound"
    );
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow::anyhow!("native configuration is not UTF-8"))?;
    let value = toml::from_str(text)
        .map_err(|_| anyhow::anyhow!("invalid native configuration; details suppressed"))?;
    Ok((value, bytes))
}
impl Profile {
    pub fn load(home: &Path, cwd: &Path, named: Option<&str>) -> Result<Self> {
        let base_path = home.join("config.toml");
        let (base, bytes) = read(&base_path)?;
        let mut sources = vec![(base_path, bytes)];
        let overlay = if let Some(name) = named {
            identifier(name)?;
            let path = home.join(format!("{name}.config.toml"));
            let (value, bytes) = read(&path)?;
            sources.push((path, bytes));
            value
        } else {
            toml::Value::Table(toml::Table::new())
        };
        let top = |key: &str| overlay.get(key).or_else(|| base.get(key));
        ensure!(
            top("profile").is_none(),
            "legacy default profiles are not supported by this installed CLI; use --profile"
        );
        let provider = top("model_provider")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!("select an explicit verified custom provider in native config")
            })?
            .to_owned();
        identifier(&provider)?;
        ensure!(
            provider == "custom",
            "provider is not in the validated capture profile"
        );
        let provider_table = |value: &toml::Value| -> Result<toml::Table> {
            match value
                .get("model_providers")
                .and_then(|providers| providers.get(&provider))
            {
                None => Ok(toml::Table::new()),
                Some(value) => value
                    .as_table()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("invalid native provider table")),
            }
        };
        let base_provider = provider_table(&base)?;
        let profile_provider = provider_table(&overlay)?;
        let field = |key: &str| profile_provider.get(key).or_else(|| base_provider.get(key));
        ensure!(
            field("wire_api").and_then(toml::Value::as_str) == Some("responses"),
            "capture profile requires native Responses wire_api"
        );
        ensure!(
            field("requires_openai_auth").and_then(toml::Value::as_bool) == Some(false),
            "OpenAI login capture is not validated for this profile"
        );
        for key in ["env_key", "env_http_headers", "auth", "aws"] {
            ensure!(
                field(key).is_none(),
                "additional provider authentication source requires validation"
            );
        }
        ensure!(
            field("supports_websockets").is_none_or(|v| v.as_bool() == Some(false)),
            "this real provider's WebSocket profile has not been validated; native transport will not be changed"
        );
        let upstream = Upstream::parse(
            field("base_url")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("explicit native upstream is required"))?,
        )?;
        let secret = field("experimental_bearer_token")
            .and_then(toml::Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("validated custom bearer authentication is required"))?;
        let mut secrets = vec![secret.to_owned()];
        // Retaining extra lower-precedence values for redaction cannot change
        // authentication; the CLI alone chooses what to send upstream.
        for table in [&base_provider, &profile_provider] {
            if let Some(value) = table
                .get("experimental_bearer_token")
                .and_then(toml::Value::as_str)
                && !value.is_empty()
                && value != secret
            {
                secrets.push(value.to_owned());
            }
            for key in ["http_headers", "query_params"] {
                if let Some(value) = table.get(key) {
                    let table = value
                        .as_table()
                        .ok_or_else(|| anyhow::anyhow!("invalid provider metadata table"))?;
                    for value in table.values() {
                        let value = value
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("invalid provider metadata value"))?;
                        if !value.is_empty() {
                            secrets.push(value.to_owned());
                        }
                    }
                }
            }
        }
        let mut auth_config = toml::Table::new();
        if let Some(store) = top("cli_auth_credentials_store") {
            auth_config.insert("cli_auth_credentials_store".into(), store.clone());
        }
        config_sources::inspect(home, &toml::Value::Table(auth_config))?.require_verified()?;
        // Project routing is filtered by the installed CLI. Auth-store changes
        // are not in that denylist, so reject them until separately verified.
        for parent in cwd.ancestors().take(64) {
            let path = parent.join(".codex/config.toml");
            if path == home.join("config.toml") {
                continue;
            }
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {
                    let (config, bytes) = read(&path)?;
                    ensure!(
                        config.get("cli_auth_credentials_store").is_none(),
                        "project authentication-store overrides require a verified adapter"
                    );
                    sources.push((path, bytes));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => bail!("project configuration could not be checked"),
            }
        }
        Ok(Self {
            provider,
            upstream,
            policy: RedactionPolicy::new(secrets)?,
            sources,
        })
    }
    pub fn unchanged(&self) -> Result<()> {
        for (path, before) in &self.sources {
            let (_, after) = read(path)?;
            ensure!(
                &after == before,
                "native configuration changed during startup; retry from the current settings"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(url: &str) -> String {
        format!(
            r#"model="gpt-6-astra"
model_provider="custom"
[model_providers.custom]
base_url="{url}"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-private-profile"
"#
        )
    }
    #[test]
    fn base_and_named_route_are_read_only_and_do_not_select_project_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let cwd = dir.path().join("project");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir_all(cwd.join(".codex")).unwrap();
        let base = config("http://127.0.0.1:1/base/v1");
        std::fs::write(home.join("config.toml"), &base).unwrap();
        std::fs::write(
            home.join("named.config.toml"),
            "[model_providers.custom]\nbase_url=\"http://127.0.0.1:2/profile/v1\"\nexperimental_bearer_token=\"synthetic-overlay-token\"\n",
        )
        .unwrap();
        std::fs::write(
            cwd.join(".codex/config.toml"),
            config("http://127.0.0.1:3/project/v1"),
        )
        .unwrap();
        let profile = Profile::load(&home, &cwd, Some("named")).unwrap();
        assert_eq!(profile.upstream.base_path(), "/profile/v1");
        profile.unchanged().unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            base
        );
        assert!(
            !profile
                .policy
                .scrub("synthetic-private-profile")
                .as_str()
                .contains("synthetic-private-profile")
        );
        assert!(
            !profile
                .policy
                .scrub("synthetic-overlay-token")
                .as_str()
                .contains("synthetic-overlay-token")
        );
        std::fs::write(home.join("named.config.toml"), "model=\"changed\"").unwrap();
        assert!(profile.unchanged().is_err());
    }
    #[test]
    fn unverified_sources_transport_and_invalid_config_never_escape_as_raw_errors() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        for suffix in [
            "env_key=\"SECRET_ENV\"",
            "auth={command=\"private-command\"}",
            "supports_websockets=true",
        ] {
            std::fs::write(
                dir.path().join("config.toml"),
                format!("{}\n{suffix}\n", config("http://127.0.0.1:1/v1")),
            )
            .unwrap();
            let error = Profile::load(dir.path(), &cwd, None)
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("SECRET_ENV") && !error.contains("private-command"));
        }
        std::fs::write(
            dir.path().join("config.toml"),
            "private-secret = [ malformed",
        )
        .unwrap();
        assert!(
            !Profile::load(dir.path(), &cwd, None)
                .err()
                .unwrap()
                .to_string()
                .contains("private-secret")
        );
        for name in ["../outside", "/absolute", "dot.name", "", "with space"] {
            assert!(identifier(name).is_err());
        }
    }
}
