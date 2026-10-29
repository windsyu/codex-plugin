//! Route/redaction adapter; configuration merging and validation belong to CLI.
use super::super::{config_sources, proxy::Upstream, redaction::RedactionPolicy};
use anyhow::{Result, ensure};
use std::path::Path;
use std::sync::Arc;

pub(super) async fn resolve_capture(
    binary: &Path,
    home: &Path,
    cwd: &Path,
    named: Option<&str>,
) -> Result<Option<Profile>> {
    let selected = async {
        let route = super::native_route::read(binary, home, cwd, named).await?;
        let mut profile = Profile::from_config(home, &route.provider, &route.url, &route.config)?;
        if let Some(binding) = &route.workspace {
            let guard = super::route_guard::Guard::new(binary, home, cwd, named, &route);
            profile.upstream = profile
                .upstream
                .with_workspace_route(binding.account_id.clone(), binding.routing_override.clone())?
                .with_workspace_guard(guard);
        }
        profile.route_snapshot = Some(route.snapshot);
        Ok::<_, anyhow::Error>(profile)
    }
    .await;
    match selected {
        Ok(profile) => Ok(Some(profile)),
        Err(_) => {
            eprintln!("codex-view: 原生模型路由暂不可读取；CLI 将自行校验配置。模型捕获不可用。");
            Ok(None)
        }
    }
}

pub(super) struct Profile {
    pub provider: String,
    pub upstream: Upstream,
    pub policy: Arc<RedactionPolicy>,
    route_snapshot: Option<super::route_guard::Snapshot>,
}

/// Named profiles are native file names. This check prevents path traversal;
/// provider identifiers use the CLI override syntax check in native_route.
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

impl Profile {
    fn from_config(home: &Path, provider: &str, route: &str, config: &toml::Value) -> Result<Self> {
        let upstream = Upstream::parse(route)?;
        let mut secrets = Vec::new();
        // Credential values are collected only for redaction, never selected or
        // validated. Command/keyring authentication remains wholly with Codex.
        fn collect(config: &toml::Value, secrets: &mut Vec<String>) {
            if let Some(table) = config.as_table() {
                for (key, value) in table {
                    if key == "experimental_bearer_token" {
                        if let Some(value) = value.as_str().filter(|v| !v.is_empty()) {
                            secrets.push(value.to_owned());
                        }
                    } else if key == "env_key" {
                        if let Some(value) = value
                            .as_str()
                            .and_then(|name| std::env::var(name).ok())
                            .filter(|v| !v.is_empty())
                        {
                            secrets.push(value);
                        }
                    } else if matches!(
                        key.as_str(),
                        "http_headers" | "query_params" | "env_http_headers"
                    ) {
                        if let Some(values) = value.as_table() {
                            for value in values.values().filter_map(toml::Value::as_str) {
                                if key == "env_http_headers" {
                                    if let Ok(value) = std::env::var(value)
                                        && !value.is_empty()
                                    {
                                        secrets.push(value);
                                    }
                                } else if !value.is_empty() {
                                    secrets.push(value.to_owned());
                                }
                            }
                        }
                    } else {
                        collect(value, secrets);
                    }
                }
            }
        }
        if provider != "openai"
            && let Some(selected) = config.get("model_providers").and_then(|v| v.get(provider))
        {
            collect(selected, &mut secrets);
        }
        let auth = config_sources::AuthSnapshot::read(home).ok();
        if let Some(auth) = &auth {
            secrets.extend(auth.redaction_secrets());
        }
        for name in ["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"] {
            if let Ok(value) = std::env::var(name)
                && !value.is_empty()
            {
                secrets.push(value);
            }
        }
        secrets.sort();
        secrets.dedup();
        Ok(Self {
            provider: provider.to_owned(),
            upstream,
            policy: RedactionPolicy::new(secrets)?,
            route_snapshot: None,
        })
    }
    pub fn child_override(&self, url: String) -> String {
        let key = if self.provider == "openai" {
            "openai_base_url".to_owned()
        } else {
            format!("model_providers.{}.base_url", self.provider)
        };
        format!("{key}={}", toml::Value::String(url))
    }
    pub fn unchanged(&self) -> Result<()> {
        if let Some(snapshot) = &self.route_snapshot {
            ensure!(
                snapshot.unchanged(),
                "native route sources changed during startup"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> toml::Value {
        toml::toml! {
            model_provider = "gateway"
            [model_providers.gateway]
            base_url = "https://model.example/v1"
            experimental_bearer_token = "synthetic-provider-token"
            http_headers = { secret = "synthetic-header-value" }
        }
        .into()
    }
    #[test]
    fn native_metadata_supplies_route_and_selected_secrets_without_auth_validation() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("auth.json"),r#"{"auth_mode":"chatgpt","tokens":{"access_token":"synthetic-access","refresh_token":"synthetic-refresh"}}"#).unwrap();
        let mut config = config();
        let selected = config["model_providers"]["gateway"].as_table_mut().unwrap();
        selected.insert("env_key".into(), "NATIVE_AUTH_ENV".into());
        selected.insert("supports_websockets".into(), true.into());
        selected.insert("future".into(), "native-only".into());
        let profile = Profile::from_config(
            directory.path(),
            "gateway",
            "https://model.example/v1",
            &config,
        )
        .unwrap();
        assert_eq!(profile.upstream.base_path(), "/v1");
        let safe = profile.policy.scrub(
            "synthetic-provider-token synthetic-header-value synthetic-access synthetic-refresh",
        );
        assert!(!safe.as_str().contains("synthetic-"));
    }
    #[test]
    fn unrelated_provider_secrets_cannot_exhaust_selected_capture_budget() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = config();
        let headers = (0..40)
            .map(|index| {
                (
                    format!("header-{index}"),
                    toml::Value::String(format!("unselected-{index}")),
                )
            })
            .collect();
        config["model_providers"].as_table_mut().unwrap().insert(
            "unused".into(),
            toml::Value::Table(toml::Table::from_iter([(
                "http_headers".into(),
                toml::Value::Table(headers),
            )])),
        );
        assert!(
            Profile::from_config(
                directory.path(),
                "gateway",
                "https://model.example/v1",
                &config
            )
            .is_ok()
        );
    }
    #[test]
    fn child_route_override_preserves_builtin_provider_and_uses_literal_custom_key() {
        let directory = tempfile::tempdir().unwrap();
        for (provider, expected) in [
            ("openai", "openai_base_url"),
            ("gateway", "model_providers.gateway.base_url"),
            ("网关 edge", "model_providers.网关 edge.base_url"),
        ] {
            let profile = Profile::from_config(
                directory.path(),
                provider,
                "https://model.example/v1",
                &config(),
            )
            .unwrap();
            assert_eq!(
                profile.child_override("http://127.0.0.1:123/cap/v1".into()),
                format!("{expected}=\"http://127.0.0.1:123/cap/v1\"")
            );
        }
    }
    #[test]
    fn startup_fence_rejects_configuration_or_auth_generation_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, "original").unwrap();
        let mut profile = Profile::from_config(
            directory.path(),
            "gateway",
            "https://model.example/v1",
            &config(),
        )
        .unwrap();
        profile.route_snapshot =
            Some(super::super::route_guard::Snapshot::read(std::slice::from_ref(&path)).unwrap());
        profile.unchanged().unwrap();
        std::fs::write(&path, "changed").unwrap();
        assert!(profile.unchanged().is_err());
    }
    #[test]
    fn unsafe_destination_and_named_profile_paths_remain_rejected() {
        let directory = tempfile::tempdir().unwrap();
        for route in [
            "http://remote.example/v1",
            "https://secret@model.example/v1",
            "https://model.example/v1?token=private",
        ] {
            assert!(Profile::from_config(directory.path(), "gateway", route, &config()).is_err());
        }
        for name in ["../outside", "/absolute", "", "with space"] {
            assert!(identifier(name).is_err());
        }
    }
}
