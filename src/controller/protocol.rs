//! App Server protocol facts that gate V2 capability exposure.

use std::path::Path;

use serde_json::{Value, json};

pub(crate) const EXPERIMENTAL_API_ENABLED: bool = true;

pub(crate) const STABLE_CATALOG_METHODS: &[&str] = &[
    "model/list",
    "permissionProfile/list",
    "mcpServerStatus/list",
    "account/usage/read",
    "account/rateLimits/read",
];

pub(crate) const EXPERIMENTAL_CATALOG_METHODS: &[&str] = &["collaborationMode/list"];

pub(crate) const SERVER_REQUEST_METHODS: &[&str] = &[
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/permissions/requestApproval",
    "item/tool/requestUserInput",
    "mcpServer/elicitation/request",
];

pub(crate) fn catalog_request_params(method: &str, cwd: &Path) -> Option<Value> {
    match method {
        "model/list" => Some(json!({"limit":100})),
        "permissionProfile/list" => Some(json!({"cwd":cwd,"limit":100})),
        "mcpServerStatus/list" => Some(json!({"limit":100})),
        "account/usage/read" | "account/rateLimits/read" => Some(Value::Null),
        "collaborationMode/list" => Some(json!({})),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use anyhow::{Context, Result};
    use serde_json::Value;

    use super::*;

    #[test]
    fn controller_fixture_opts_in_and_covers_catalog_detection() -> Result<()> {
        let messages = include_str!("../../fixtures/app-server-v2-controller-init.jsonl")
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<serde_json::Result<Vec<_>>>()?;
        let initialize = messages
            .iter()
            .find(|entry| entry["message"]["method"] == "initialize")
            .context("fixture lacks initialize")?;
        assert_eq!(
            initialize["message"]["params"]["capabilities"]["experimentalApi"],
            EXPERIMENTAL_API_ENABLED
        );

        let client_methods = messages
            .iter()
            .filter(|entry| entry["direction"] == "client_to_server")
            .filter_map(|entry| entry["message"]["method"].as_str())
            .collect::<BTreeSet<_>>();
        for method in STABLE_CATALOG_METHODS
            .iter()
            .chain(EXPERIMENTAL_CATALOG_METHODS)
        {
            assert!(client_methods.contains(method), "fixture lacks {method}");
        }
        for method in STABLE_CATALOG_METHODS
            .iter()
            .chain(EXPERIMENTAL_CATALOG_METHODS)
        {
            let request = messages
                .iter()
                .find(|entry| entry["message"]["method"] == *method)
                .with_context(|| format!("fixture lacks request {method}"))?;
            assert_eq!(
                request["message"]["params"],
                catalog_request_params(method, Path::new("/synthetic/workspace"))
                    .context("catalog method lacks params")?
            );
        }
        Ok(())
    }

    #[test]
    fn compatibility_manifest_matches_generated_codex_schema() -> Result<()> {
        let manifest: Value =
            serde_json::from_str(include_str!("../../compatibility/codex-0.149.1-v2.json"))?;
        assert_eq!(manifest["testedCodexCliVersion"], "0.149.1");
        assert_eq!(manifest["initialize"]["experimentalApi"], true);
        assert_eq!(manifest["controllerDefaultEnabled"], false);
        assert_eq!(
            manifest["schema"]["experimentalBundleSha256"],
            "4f4a8d8f53f971b97f818639f58c8d26bb68bfcdfa2d2f20572cb97e6761ab91"
        );
        Ok(())
    }

    #[test]
    fn conversation_fixture_covers_only_the_published_typed_slice() -> Result<()> {
        let messages = include_str!("../../fixtures/app-server-v2-conversation.jsonl")
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<serde_json::Result<Vec<_>>>()?;
        let methods = messages
            .iter()
            .filter(|entry| entry["direction"] == "client_to_server")
            .filter_map(|entry| entry["message"]["method"].as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            methods,
            BTreeSet::from([
                "review/start",
                "thread/archive",
                "thread/compact/start",
                "thread/fork",
                "thread/goal/clear",
                "thread/goal/get",
                "thread/goal/set",
                "thread/name/set",
                "thread/resume",
                "thread/settings/update",
                "thread/start",
                "turn/interrupt",
                "turn/start",
                "turn/steer",
            ])
        );
        assert!(messages.iter().all(|entry| {
            entry["direction"] == "client_to_server" || entry["direction"] == "server_to_client"
        }));
        let plan = messages
            .iter()
            .find(|entry| {
                entry["message"]["method"] == "thread/settings/update"
                    && entry["message"]["params"]["collaborationMode"]["mode"] == "plan"
            })
            .context("fixture lacks Plan settings update")?;
        assert_eq!(
            plan["message"]["params"]["collaborationMode"]["settings"],
            json!({
                "model":"synthetic-model",
                "reasoning_effort":"high",
                "developer_instructions":null,
            })
        );
        let goal_statuses = messages
            .iter()
            .filter(|entry| entry["message"]["method"] == "thread/goal/set")
            .filter_map(|entry| entry["message"]["params"]["status"].as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(goal_statuses, BTreeSet::from(["active", "paused"]));
        assert!(messages.iter().any(|entry| {
            entry["message"]["method"] == "thread/goal/clear"
                && entry["direction"] == "client_to_server"
        }));
        let server_request_methods = messages
            .iter()
            .filter(|entry| entry["direction"] == "server_to_client")
            .filter(|entry| entry["message"].get("id").is_some())
            .filter_map(|entry| entry["message"]["method"].as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            server_request_methods,
            SERVER_REQUEST_METHODS.iter().copied().collect()
        );
        assert!(messages.iter().any(|entry| {
            entry["direction"] == "server_to_client"
                && entry["message"]["method"] == "serverRequest/resolved"
        }));
        Ok(())
    }
}
