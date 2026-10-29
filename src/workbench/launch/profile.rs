//! Version-scoped route adapter, not an implementation of Codex config merging.
//! Only the tested base/named-user profile route fields are selected here; the
//! original files, project settings, permissions and credentials stay with CLI.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::super::{config_sources, proxy::Upstream, redaction::RedactionPolicy};
use super::failure::LaunchFailure;
use anyhow::{Result, bail, ensure};

const CONFIG_BYTES: u64 = 2 * 1024 * 1024;

/// Select capture only for the already supported explicit custom route. All
/// other native configurations run unchanged, including configurations whose
/// syntax or sources this bounded probe cannot understand. The CLI owns their
/// validation. In particular, an unselected custom table says nothing about the
/// effective default OpenAI route or its authentication.
pub(super) fn select_capture(
    home: &Path,
    cwd: &Path,
    named: Option<&str>,
) -> Result<Option<Profile>> {
    let probe = || -> Result<bool> {
        let (base, _) = read(&home.join("config.toml"))?;
        let overlay = if let Some(name) = named {
            identifier(name)?;
            read(&home.join(format!("{name}.config.toml")))?.0
        } else {
            toml::Value::Table(toml::Table::new())
        };
        let top = |key: &str| overlay.get(key).or_else(|| base.get(key));
        Ok(top("profile").is_none()
            && top("model_provider").and_then(toml::Value::as_str) == Some("custom"))
    };
    if !probe().unwrap_or(false) {
        return Ok(None);
    }
    Profile::load(home, cwd, named)
        .map(Some)
        .map_err(|error| LaunchFailure::CaptureConfig.wrap(error))
}

pub(super) struct Profile {
    pub provider: String,
    pub upstream: Upstream,
    pub policy: Arc<RedactionPolicy>,
    sources: Vec<(PathBuf, Vec<u8>)>,
    auth: config_sources::AuthSnapshot,
    home: PathBuf,
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
        let (base, bytes) = read(&base_path).map_err(|error| LaunchFailure::Config.wrap(error))?;
        let mut sources = vec![(base_path, bytes)];
        let overlay = if let Some(name) = named {
            identifier(name)?;
            let path = home.join(format!("{name}.config.toml"));
            let (value, bytes) = read(&path).map_err(|error| LaunchFailure::Config.wrap(error))?;
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
        ensure!(provider == "custom", LaunchFailure::Provider);
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
            field("wire_api").is_none_or(|value| value.as_str() == Some("responses")),
            "capture profile requires native Responses wire_api"
        );
        let requires_openai_auth = field("requires_openai_auth")
            .map(|value| value.as_bool().ok_or(LaunchFailure::Auth))
            .transpose()?
            .unwrap_or(false);
        // Only authentication precedence affects the capture route. The CLI
        // validates and executes the command, args, cwd and refresh settings.
        let command_auth = field("auth").is_some();
        if command_auth {
            // The CLI owns command execution, caching and retry. True would
            // additionally select the unrelated native OpenAI account manager.
            ensure!(!requires_openai_auth, LaunchFailure::Auth);
        }
        // This flag permits native API-key authentication as well as ChatGPT.
        // The explicit provider bearer below retains native precedence in both
        // cases; source inspection still excludes cloud login and managed auth.
        for key in ["env_key", "env_http_headers", "aws"] {
            ensure!(field(key).is_none(), LaunchFailure::Auth);
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
        let static_bearer = field("experimental_bearer_token")
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .ok_or(LaunchFailure::Auth)
            })
            .transpose()?;
        ensure!(command_auth ^ static_bearer.is_some(), LaunchFailure::Auth);
        let mut secrets: Vec<String> = static_bearer.into_iter().map(str::to_owned).collect();
        // Retaining extra lower-precedence values for redaction cannot change
        // authentication; the CLI alone chooses what to send upstream.
        for table in [&base_provider, &profile_provider] {
            if let Some(value) = table
                .get("experimental_bearer_token")
                .and_then(toml::Value::as_str)
                && !value.is_empty()
                && !secrets.iter().any(|secret| secret == value)
            {
                secrets.push(value.to_owned());
            }
            for key in ["http_headers", "query_params"] {
                if let Some(values) = table.get(key).and_then(toml::Value::as_table) {
                    secrets.extend(
                        values
                            .values()
                            .filter_map(toml::Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(str::to_owned),
                    );
                }
            }
        }
        let mut auth_config = toml::Table::new();
        if let Some(store) = top("cli_auth_credentials_store") {
            auth_config.insert("cli_auth_credentials_store".into(), store.clone());
        }
        // Native login state does not supply the command-backed model token,
        // but it can independently enable a managed configuration bundle that
        // changes omitted provider fields. Preserve the effective-route gate
        // and startup snapshot without ever acquiring the command token.
        let (sources_checked, auth) =
            config_sources::inspect_for_launch(home, &toml::Value::Table(auth_config))
                .map_err(|e| LaunchFailure::Auth.wrap(e))?;
        sources_checked
            .require_verified()
            .map_err(|e| LaunchFailure::Auth.wrap(e))?;
        secrets.extend(auth.redaction_secrets());
        // Project routing is filtered by the installed CLI. Auth-store changes
        // are not in that denylist, so reject them until separately verified.
        for parent in cwd.ancestors().take(64) {
            let path = parent.join(".codex/config.toml");
            if path == home.join("config.toml") {
                continue;
            }
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {
                    let (config, bytes) =
                        read(&path).map_err(|e| LaunchFailure::ProjectConfig.wrap(e))?;
                    ensure!(
                        config.get("cli_auth_credentials_store").is_none(),
                        LaunchFailure::ProjectConfig
                    );
                    sources.push((path, bytes));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => bail!(LaunchFailure::ProjectConfig),
            }
        }
        Ok(Self {
            provider,
            upstream,
            policy: RedactionPolicy::new(secrets)?,
            sources,
            auth,
            home: home.to_path_buf(),
        })
    }
    pub fn unchanged(&self) -> Result<()> {
        self.auth.unchanged(&self.home)?;
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
    #[test]
    fn omitted_provider_keeps_native_route_and_ignores_unselected_custom_fields() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(
            directory.path().join("config.toml"),
            r#"
model="native-model"
future_setting={enabled=true}
[model_providers.custom]
base_url="https://unused.example/v1"
wire_api="responses"
experimental_bearer_token="synthetic-unused"
"#,
        )
        .unwrap();
        // A directory would fail AuthSnapshot::read: direct mode must not read it.
        std::fs::create_dir(directory.path().join("auth.json")).unwrap();
        assert!(
            select_capture(directory.path(), &cwd, None)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn direct_launch_delegates_missing_unknown_and_malformed_native_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        assert!(
            select_capture(directory.path(), &cwd, None)
                .unwrap()
                .is_none()
        );
        for config in [
            "",
            "model_provider='openai'",
            "model_provider='future-provider'\nfuture={enabled=true}",
            "model_provider = [ malformed",
        ] {
            std::fs::write(directory.path().join("config.toml"), config).unwrap();
            assert!(
                select_capture(directory.path(), &cwd, None)
                    .unwrap()
                    .is_none()
            );
        }
        std::fs::write(
            directory.path().join("config.toml"),
            config("https://example.invalid/v1"),
        )
        .unwrap();
        std::fs::write(
            directory.path().join("native.config.toml"),
            "model_provider='openai'",
        )
        .unwrap();
        assert!(
            select_capture(directory.path(), &cwd, Some("native"))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn capture_accepts_optional_native_defaults_and_unrelated_future_settings() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let config = config("https://example.invalid/v1")
            .replace("wire_api=\"responses\"\n", "")
            .replace("requires_openai_auth=false\n", "");
        std::fs::write(directory.path().join("config.toml"), format!("future_setting={{enabled=true}}\n{config}future_provider_setting={{mode='future'}}\n")).unwrap();
        let profile = select_capture(directory.path(), &cwd, None)
            .unwrap()
            .unwrap();
        assert_eq!(profile.provider, "custom");
        profile.unchanged().unwrap();
    }
    #[test]
    fn capture_leaves_command_arguments_and_metadata_schema_to_native_cli() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let config = command_config("https://example.invalid/v1").replace(
            "args=[\"private-command-argument\"]",
            "args='native-validation-owned'",
        );
        std::fs::write(
            directory.path().join("config.toml"),
            format!("{config}http_headers={{example=42}}\nfuture={{enabled=true}}\n"),
        )
        .unwrap();
        assert!(
            select_capture(directory.path(), &cwd, None)
                .unwrap()
                .is_some()
        );
    }
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
    fn command_config(url: &str) -> String {
        format!(
            r#"model="gpt-6-astra"
model_provider="custom"
cli_auth_credentials_store="file"
[model_providers.custom]
base_url="{url}"
wire_api="responses"
requires_openai_auth=false
auth={{command="/synthetic/not-executed",args=["private-command-argument"]}}
"#
        )
    }
    #[test]
    fn explicit_bearer_accepts_native_api_key_auth_without_changing_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let cwd = directory.path().join("new project");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&cwd).unwrap();
        let config = config("http://127.0.0.1:1/v1")
            .replace("requires_openai_auth=false", "requires_openai_auth=true");
        let auth = r#"{"auth_mode":"apikey","OPENAI_API_KEY":"synthetic-native-api-key"}"#;
        std::fs::write(home.join("config.toml"), &config).unwrap();
        std::fs::write(home.join("auth.json"), auth).unwrap();
        let profile = Profile::load(&home, &cwd, None).unwrap();
        profile.unchanged().unwrap();
        let scrubbed = profile
            .policy
            .scrub("synthetic-private-profile synthetic-native-api-key");
        assert!(!scrubbed.as_str().contains("synthetic-private-profile"));
        assert!(!scrubbed.as_str().contains("synthetic-native-api-key"));
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            config
        );
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            auth
        );
        std::fs::write(
            home.join("auth.json"),
            r#"{"tokens":{"access_token":"synthetic-chatgpt"}}"#,
        )
        .unwrap();
        assert!(profile.unchanged().is_err());
        assert!(Profile::load(&home, &cwd, None).is_err());
    }
    #[test]
    fn command_auth_is_left_to_cli_while_native_login_state_only_guards_route_policy() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let cwd = directory.path().join("project");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&cwd).unwrap();
        let config = command_config("http://127.0.0.1:1/v1");
        std::fs::write(home.join("config.toml"), &config).unwrap();

        let profile = Profile::load(&home, &cwd, None).unwrap();
        assert_eq!(profile.upstream.base_path(), "/v1");
        profile.unchanged().unwrap();

        let native_key = "synthetic-native-api-key";
        std::fs::write(
            home.join("auth.json"),
            format!(r#"{{"auth_mode":"apikey","OPENAI_API_KEY":"{native_key}"}}"#),
        )
        .unwrap();
        assert!(profile.unchanged().is_err());
        let profile = Profile::load(&home, &cwd, None).unwrap();
        assert!(
            !profile
                .policy
                .scrub(native_key)
                .as_str()
                .contains(native_key)
        );
        profile.unchanged().unwrap();

        std::fs::write(
            home.join("auth.json"),
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"unused-private-token"}}"#,
        )
        .unwrap();
        assert!(profile.unchanged().is_err());
        assert!(Profile::load(&home, &cwd, None).is_err());

        std::fs::write(
            home.join("config.toml"),
            config.replace("/v1", "/changed/v1"),
        )
        .unwrap();
        assert!(profile.unchanged().is_err());
    }

    #[test]
    fn command_auth_rejects_ambiguous_credentials_without_exposing_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let base = command_config("http://127.0.0.1:1/v1");
        for config in [
            base.replace("requires_openai_auth=false", "requires_openai_auth=true"),
            format!("{base}experimental_bearer_token=\"private-static-token\"\n"),
            format!("{base}experimental_bearer_token=\"\"\n"),
            format!("{base}experimental_bearer_token=1\n"),
            format!("{base}env_key=\"PRIVATE_ENV\"\n"),
            format!("{base}env_http_headers={{x=\"PRIVATE_HEADER\"}}\n"),
            format!("{base}aws={{profile=\"PRIVATE_AWS\"}}\n"),
            base.replace(
                "cli_auth_credentials_store=\"file\"",
                "cli_auth_credentials_store=\"keyring\"",
            ),
        ] {
            std::fs::write(directory.path().join("config.toml"), config).unwrap();
            let error = Profile::load(directory.path(), &cwd, None)
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("private") && !error.contains("synthetic/not-executed"));
        }
    }

    #[test]
    fn named_profile_recursively_overlays_command_auth_members() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let cwd = directory.path().join("project");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(
            home.join("config.toml"),
            command_config("http://127.0.0.1:1/v1"),
        )
        .unwrap();
        for overlay in [
            "[model_providers.custom.auth]\nargs=[\"named-private-argument\"]\n",
            "[model_providers.custom.auth]\nrefresh_interval_ms=0\n",
        ] {
            std::fs::write(home.join("named.config.toml"), overlay).unwrap();
            let profile = Profile::load(&home, &cwd, Some("named")).unwrap();
            assert_eq!(profile.upstream.base_path(), "/v1");
            profile.unchanged().unwrap();
        }
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
        for openai_auth in [false, true] {
            for suffix in [
                "env_key=\"SECRET_ENV\"",
                "auth={command=\"private-command\"}",
                "supports_websockets=true",
            ] {
                let base = config("http://127.0.0.1:1/v1").replace(
                    "requires_openai_auth=false",
                    &format!("requires_openai_auth={openai_auth}"),
                );
                std::fs::write(
                    dir.path().join("config.toml"),
                    format!("{base}\n{suffix}\n"),
                )
                .unwrap();
                let error = Profile::load(dir.path(), &cwd, None)
                    .err()
                    .unwrap()
                    .to_string();
                assert!(!error.contains("SECRET_ENV") && !error.contains("private-command"));
            }
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
