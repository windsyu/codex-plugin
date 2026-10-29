//! Bounded, version-scoped reads from the installed CLI. No thread is created;
//! the actual interactive process remains the ordinary native TUI.
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub(super) struct WorkspaceRoute {
    pub account_id: String,
    pub routing_override: Option<String>,
}
pub(super) struct Route {
    pub provider: String,
    pub url: String,
    pub config: toml::Value,
    pub workspace: Option<WorkspaceRoute>,
    pub snapshot: super::route_guard::Snapshot,
}

pub(super) async fn read(
    binary: &Path,
    home: &Path,
    cwd: &Path,
    named: Option<&str>,
) -> Result<Route> {
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        read_inner(binary, home, cwd, named),
    )
    .await
    .map_err(|_| anyhow::anyhow!("native route lookup timed out"))?
}
async fn read_inner(binary: &Path, home: &Path, cwd: &Path, named: Option<&str>) -> Result<Route> {
    let mut command = tokio::process::Command::new(binary);
    command
        .current_dir(cwd)
        .env("CODEX_HOME", home)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(name) = named {
        command.args(["--profile", name]);
    }
    // Avoid unrelated plugin discovery in this short-lived metadata reader.
    command.args(["-c", "features.plugins=false", "app-server", "--stdio"]);
    use std::os::unix::process::CommandExt;
    command.as_std_mut().process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| anyhow::anyhow!("native route helper failed to start"))?;
    struct ProcessGroup(Option<u32>);
    impl ProcessGroup {
        fn stop(&mut self) {
            if let Some(pid) = self.0.take() {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
        }
    }
    impl Drop for ProcessGroup {
        fn drop(&mut self) {
            self.stop();
        }
    }
    let mut group = ProcessGroup(child.id());
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    async fn send(input: &mut tokio::process::ChildStdin, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        input.write_all(&bytes).await?;
        input.flush().await?;
        Ok(())
    }
    async fn response(
        output: &mut BufReader<tokio::process::ChildStdout>,
        id: u64,
    ) -> Result<Value> {
        let mut budget = 8 * 1024 * 1024;
        loop {
            let mut bytes = Vec::new();
            // read_until alone would permit an unbounded line from a damaged executable.
            loop {
                let available = output.fill_buf().await?;
                ensure!(!available.is_empty(), "native route helper closed");
                let size = available
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(available.len(), |p| p + 1);
                ensure!(
                    size <= budget && bytes.len() + size <= 2 * 1024 * 1024,
                    "native route response exceeds bound"
                );
                bytes.extend_from_slice(&available[..size]);
                budget -= size;
                output.consume(size);
                if bytes.last() == Some(&b'\n') {
                    break;
                }
            }
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("invalid native route response"))?;
            if value["id"] == id {
                ensure!(
                    value.get("error").is_none(),
                    "native route lookup request {} rejected (code {})",
                    id,
                    value["error"]["code"].as_i64().unwrap_or_default()
                );
                return Ok(value["result"].clone());
            }
        }
    }
    send(&mut input, json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"codex_view_route_reader","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}})).await?;
    response(&mut output, 1).await?;
    send(&mut input, json!({"method":"initialized"})).await?;
    send(
        &mut input,
        json!({"id":2,"method":"config/read","params":{"includeLayers":true,"cwd":cwd}}),
    )
    .await?;
    let config_response = response(&mut output, 2).await?;
    let config = config_response["config"].clone();
    let mut source_paths = vec![home.join("auth.json"), home.join("config.toml")];
    if let Some(name) = named {
        super::profile::identifier(name)?;
        source_paths.push(home.join(format!("{name}.config.toml")));
    }
    for layer in config_response["layers"].as_array().into_iter().flatten() {
        let name = &layer["name"];
        match name["type"].as_str() {
            Some("sessionFlags") => {}
            Some("project") => {
                let directory = name["dotCodexFolder"]
                    .as_str()
                    .or_else(|| name["dot_codex_folder"].as_str())
                    .ok_or_else(|| anyhow::anyhow!("invalid native source"))?;
                source_paths.push(Path::new(directory).join("config.toml"));
            }
            Some("user" | "system" | "packagedDefaults" | "legacyManagedConfigTomlFromFile") => {
                source_paths.push(
                    name["file"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("invalid native source"))?
                        .into(),
                );
            }
            _ => bail!("native non-file route source cannot be fenced"),
        }
    }
    // Preserve absence as well as presence: newly created native source files
    // must invalidate a route just like editing a source returned by config/read.
    for name in ["config.toml", "requirements.toml", "managed_config.toml"] {
        source_paths.push(Path::new("/etc/codex").join(name));
    }
    for name in ["requirements.toml", "managed_config.toml"] {
        source_paths.push(home.join(name));
    }
    ensure!(
        cwd.ancestors().count() <= 64,
        "native project ancestry exceeds inspection bound"
    );
    source_paths.extend(
        cwd.ancestors()
            .map(|parent| parent.join(".codex/config.toml")),
    );
    source_paths.sort();
    source_paths.dedup();
    let snapshot = super::route_guard::Snapshot::read(&source_paths)?;
    send(
        &mut input,
        json!({"id":3,"method":"configRequirements/read"}),
    )
    .await?;
    let requirements = response(&mut output, 3).await?["requirements"].clone();
    send(
        &mut input,
        json!({"id":4,"method":"account/read","params":{"refreshToken":false}}),
    )
    .await?;
    let account = response(&mut output, 4).await?;
    send(
        &mut input,
        json!({"id":5,"method":"config/read","params":{"includeLayers":true,"cwd":cwd}}),
    )
    .await?;
    ensure!(
        response(&mut output, 5).await? == config_response && snapshot.unchanged(),
        "native route sources changed during lookup"
    );
    group.stop();
    child.kill().await.ok();
    child.wait().await.ok();
    let mut route = resolve(config, requirements, account)?;
    route.snapshot = snapshot;
    Ok(route)
}

fn resolve(config: Value, requirements: Value, account: Value) -> Result<Route> {
    let provider = requirements["modelProvider"]
        .as_str()
        .or_else(|| config["model_provider"].as_str())
        .unwrap_or("openai")
        .to_owned();
    ensure!(
        requirements["modelProviders"].get(&provider).is_none(),
        "managed provider route cannot be overridden for capture"
    );
    ensure!(
        !matches!(
            provider.as_str(),
            "ollama" | "lmstudio" | "amazon-bedrock" | "amazon-bedrock-runtime"
        ),
        "built-in effective route is not exported by this CLI adapter"
    );
    ensure!(
        requirements["chatgptBaseUrl"].is_null() && requirements["modelProvider"].is_null(),
        "managed route policy cannot be fenced by native file sources"
    );
    let definition = requirements["modelProviders"]
        .get(&provider)
        .or_else(|| config["model_providers"].get(&provider));
    // SigV4 signs the destination URL itself. Replacing it with loopback and
    // restoring Host later cannot preserve that signature; leave CLI direct.
    ensure!(
        definition
            .and_then(|value| value.get("aws"))
            .is_none_or(Value::is_null),
        "signed native destination cannot be rewritten for capture"
    );
    let explicit = if provider == "openai" {
        config["openai_base_url"].as_str()
    } else {
        definition.and_then(|v| v["base_url"].as_str())
    };
    ensure!(
        !provider.is_empty() && !provider.contains(['.', '=', '\0']),
        "native provider key cannot be represented by CLI override syntax"
    );
    // Native first-party routing is a provider property, not a provider ID.
    let first_party = provider == "openai"
        || definition.is_some_and(|info| {
            info["name"] == "OpenAI"
                && info["requires_openai_auth"] == true
                && ["env_key", "experimental_bearer_token", "auth", "aws"]
                    .iter()
                    .all(|key| info[*key].is_null())
        });
    let workspace_bound = first_party
        && explicit.is_none_or(|value| {
            if !value.trim_end_matches('/').ends_with("/backend-api/codex") {
                return false;
            }
            let Ok(url) = reqwest::Url::parse(value) else {
                return false;
            };
            let bootstrap = config["chatgpt_base_url"]
                .as_str()
                .unwrap_or("https://chatgpt.com");
            let backend = account["workspaceRouting"]["backendOrigin"].as_str();
            value == "https://chatgpt.com/backend-api/codex"
                || reqwest::Url::parse(bootstrap).is_ok_and(|base| base.origin() == url.origin())
                || backend
                    .and_then(|base| reqwest::Url::parse(base).ok())
                    .is_some_and(|base| base.origin() == url.origin())
        });
    let mut workspace = None;
    let url = if let Some(route) = account
        .get("workspaceRouting")
        .filter(|v| !v.is_null() && workspace_bound)
    {
        ensure!(
            first_party,
            "native workspace route requires first-party provider authentication"
        );
        let origin = route["backendOrigin"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing native workspace origin"))?;
        let parsed =
            reqwest::Url::parse(origin).map_err(|_| anyhow::anyhow!("invalid workspace origin"))?;
        ensure!(
            parsed.scheme() == "https"
                && parsed.host_str().is_some()
                && parsed.path() == "/"
                && parsed.query().is_none()
                && parsed.fragment().is_none()
                && parsed.username().is_empty()
                && parsed.password().is_none(),
            "invalid native workspace origin"
        );
        let routing_override = match route["accountRoutingOverride"].as_str() {
            Some("NO_CONSTRAINT") => None,
            Some("us") => Some("us".into()),
            Some("us_cr") => Some("us_cr".into()),
            _ => bail!("unknown native workspace routing policy"),
        };
        let account_id = route["chatgptAccountId"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 1024)
            .ok_or_else(|| anyhow::anyhow!("missing native workspace identity"))?
            .to_owned();
        ensure!(
            config["cli_auth_credentials_store"]
                .as_str()
                .is_none_or(|mode| mode == "file"),
            "native workspace credential store has no file-generation fence"
        );
        workspace = Some(WorkspaceRoute {
            account_id,
            routing_override,
        });
        // Official routing replaces the origin while preserving the provider's
        // path. Built-in ChatGPT's path is /backend-api/codex.
        let path = explicit
            .and_then(|value| reqwest::Url::parse(value).ok())
            .map(|url| url.path().to_owned())
            .unwrap_or_else(|| "/backend-api/codex".into());
        format!("{}{path}", origin.trim_end_matches('/'))
    } else if let Some(explicit) = explicit {
        explicit.to_owned()
    } else if provider == "openai" && account["account"]["type"] == "apiKey" {
        "https://api.openai.com/v1".into()
    } else {
        bail!("native effective model route unavailable")
    };
    // Keep only credential-bearing provider metadata for redaction. Native
    // optional/null fields are neither validated nor round-tripped into config.
    fn to_toml(value: &Value) -> toml::Value {
        match value {
            Value::Object(values) => toml::Value::Table(
                values
                    .iter()
                    .filter(|(_, v)| !v.is_null())
                    .map(|(k, v)| (k.clone(), to_toml(v)))
                    .collect(),
            ),
            Value::Array(values) => toml::Value::Array(
                values
                    .iter()
                    .filter(|v| !v.is_null())
                    .map(to_toml)
                    .collect(),
            ),
            Value::String(v) => toml::Value::String(v.clone()),
            Value::Bool(v) => toml::Value::Boolean(*v),
            Value::Number(v) => v
                .as_i64()
                .map(toml::Value::Integer)
                .unwrap_or_else(|| toml::Value::Float(v.as_f64().unwrap_or_default())),
            Value::Null => toml::Value::String(String::new()),
        }
    }
    Ok(Route {
        provider,
        url,
        config: to_toml(&config),
        workspace,
        snapshot: super::route_guard::Snapshot::read(&[])?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_workspace_route_is_read_from_official_result_and_bound_to_account() {
        let route = resolve(json!({"model_provider":null}), Value::Null, json!({"workspaceRouting":{"backendOrigin":"https://workspace.example","chatgptAccountId":"synthetic-account","accountRoutingOverride":"us"}})).unwrap();
        assert_eq!(route.provider, "openai");
        assert_eq!(route.url, "https://workspace.example/backend-api/codex");
        assert_eq!(
            route.workspace.unwrap().routing_override.as_deref(),
            Some("us")
        );
    }
    #[test]
    fn ignored_openai_table_and_independent_native_endpoint_keep_native_routing() {
        let account = json!({"workspaceRouting":{"backendOrigin":"https://chatgpt.com","chatgptAccountId":"synthetic-account","accountRoutingOverride":"us"}});
        let default = resolve(
            json!({"model_providers":{"openai":{"base_url":"https://unselected.invalid/v1"}}}),
            Value::Null,
            account.clone(),
        )
        .unwrap();
        assert_eq!(default.url, "https://chatgpt.com/backend-api/codex");
        let independent = resolve(
            json!({"openai_base_url":"https://independent.example/v1"}),
            Value::Null,
            account,
        )
        .unwrap();
        assert_eq!(independent.url, "https://independent.example/v1");
        assert!(independent.workspace.is_none());
    }
    #[test]
    fn locked_routes_and_non_file_workspace_credentials_are_not_misrepresented_as_captured() {
        let config = json!({"model_provider":"gateway","model_providers":{"gateway":{"base_url":"https://native.example/v1"}}});
        assert!(
            resolve(
                config,
                json!({"modelProviders":{"gateway":{"base_url":"https://required.example/v1"}}}),
                Value::Null
            )
            .is_err()
        );
        assert!(resolve(json!({"cli_auth_credentials_store":"keyring"}),Value::Null,json!({"workspaceRouting":{"backendOrigin":"https://chatgpt.com","chatgptAccountId":"synthetic-account","accountRoutingOverride":"NO_CONSTRAINT"}})).is_err());
    }
    #[test]
    fn destination_signed_auth_is_not_rewritten_but_other_native_auth_is_not_validated() {
        let config = |auth| json!({"model_provider":"gateway","model_providers":{"gateway":{"base_url":"https://model.example/v1","aws":auth}}});
        assert!(
            resolve(
                config(json!({"region":"us-east-1"})),
                Value::Null,
                Value::Null
            )
            .is_err()
        );
        assert!(resolve(json!({"model_provider":"gateway","model_providers":{"gateway":{"base_url":"https://model.example/v1","env_key":"NATIVE_KEY","supports_websockets":true,"future":true}}}),Value::Null,Value::Null).is_ok());
    }
    #[test]
    fn provider_labels_are_not_an_ascii_name_allowlist() {
        let route = resolve(json!({"model_provider":"网关 edge","model_providers":{"网关 edge":{"base_url":"https://model.example/v1"}}}),Value::Null,Value::Null).unwrap();
        assert_eq!(route.provider, "网关 edge");
        assert!(resolve(json!({"model_provider":"ambiguous.key","model_providers":{"ambiguous.key":{"base_url":"https://model.example/v1"}}}),Value::Null,Value::Null).is_err());
    }
    #[test]
    fn native_first_party_route_is_not_limited_to_the_builtin_provider_id() {
        let account = json!({"workspaceRouting":{"backendOrigin":"https://regional.example","chatgptAccountId":"synthetic-account","accountRoutingOverride":"us"}});
        let config = |name, auth| json!({"model_provider":"gateway","model_providers":{"gateway":{"name":name,"requires_openai_auth":true,"base_url":"https://chatgpt.com/backend-api/codex","auth":auth}}});
        let route = resolve(config("OpenAI", Value::Null), Value::Null, account.clone()).unwrap();
        assert_eq!(route.provider, "gateway");
        assert_eq!(route.url, "https://regional.example/backend-api/codex");
        assert!(route.workspace.is_some());
        for config in [
            config("Independent", Value::Null),
            config("OpenAI", json!({"command":"native-command"})),
        ] {
            let route = resolve(config, Value::Null, account.clone()).unwrap();
            assert_eq!(route.url, "https://chatgpt.com/backend-api/codex");
            assert!(route.workspace.is_none());
        }
    }
    #[test]
    fn unknown_workspace_policy_and_unsafe_origins_are_never_guessed() {
        for origin in [
            "http://workspace.example",
            "https://workspace.example/private",
            "https://secret@workspace.example",
        ] {
            assert!(resolve(json!({}), Value::Null, json!({"workspaceRouting":{"backendOrigin":origin,"chatgptAccountId":"synthetic-account","accountRoutingOverride":"NO_CONSTRAINT"}})).is_err());
        }
        assert!(
            resolve(
                json!({}),
                Value::Null,
                json!({"account":{"type":"chatgpt"}})
            )
            .is_err()
        );
    }
}

#[cfg(test)]
#[path = "native_route/tests.rs"]
mod installed_tests;
