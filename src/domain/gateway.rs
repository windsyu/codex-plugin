use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingRequestAction {
    Approval {
        decision: String,
    },
    Permissions {
        grant: bool,
        scope: String,
        strict_auto_review: Option<bool>,
    },
    UserInput {
        answers: BTreeMap<String, Vec<String>>,
    },
    McpElicitation {
        action: String,
        content: Option<Value>,
    },
}

pub fn request_action_response(
    method: &str,
    payload: &Value,
    action: &PendingRequestAction,
) -> std::result::Result<(Value, &'static str), (&'static str, &'static str)> {
    match (method, action) {
        (
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval",
            PendingRequestAction::Approval { decision },
        ) => {
            if !matches!(
                decision.as_str(),
                "accept" | "acceptForSession" | "decline" | "cancel"
            ) {
                return Err(("COMMAND_INVALID", "approval decision is invalid"));
            }
            if method == "item/commandExecution/requestApproval"
                && payload
                    .get("availableDecisions")
                    .and_then(Value::as_array)
                    .is_some_and(|available| {
                        !available
                            .iter()
                            .any(|value| value.as_str() == Some(decision))
                    })
            {
                return Err((
                    "CAPABILITY_UNAVAILABLE",
                    "approval decision is not offered by the source",
                ));
            }
            Ok((json!({"decision":decision}), "approval"))
        }
        (
            "item/permissions/requestApproval",
            PendingRequestAction::Permissions {
                grant,
                scope,
                strict_auto_review,
            },
        ) => {
            if !matches!(scope.as_str(), "turn" | "session") {
                return Err(("COMMAND_INVALID", "permission grant scope is invalid"));
            }
            let permissions = if *grant {
                payload
                    .get("permissions")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
            } else {
                json!({})
            };
            Ok((
                json!({
                    "permissions":permissions,
                    "scope":scope,
                    "strictAutoReview":strict_auto_review,
                }),
                "permissions",
            ))
        }
        ("item/tool/requestUserInput", PendingRequestAction::UserInput { answers }) => {
            let questions = payload.get("questions").and_then(Value::as_array).ok_or((
                "UPSTREAM_PROTOCOL_INVALID",
                "question request lacks questions",
            ))?;
            if answers.len() != questions.len() {
                return Err(("COMMAND_INVALID", "every question requires one answer"));
            }
            for question in questions {
                let id = question
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or(("UPSTREAM_PROTOCOL_INVALID", "question request lacks an id"))?;
                let values = answers
                    .get(id)
                    .ok_or(("COMMAND_INVALID", "question answer id is invalid"))?;
                if values.is_empty() || values.len() > 20 {
                    return Err(("COMMAND_INVALID", "question answer count is invalid"));
                }
                if let Some(options) = question.get("options").and_then(Value::as_array)
                    && !question
                        .get("isOther")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    && values.iter().any(|answer| {
                        !options.iter().any(|option| {
                            option.get("label").and_then(Value::as_str) == Some(answer)
                        })
                    })
                {
                    return Err((
                        "COMMAND_INVALID",
                        "question answer is not an advertised option",
                    ));
                }
            }
            let answers = answers
                .iter()
                .map(|(id, values)| (id.clone(), json!({"answers":values})))
                .collect::<serde_json::Map<_, _>>();
            Ok((json!({"answers":answers}), "user_input"))
        }
        (
            "mcpServer/elicitation/request",
            PendingRequestAction::McpElicitation { action, content },
        ) => {
            if !matches!(action.as_str(), "accept" | "decline" | "cancel") {
                return Err(("COMMAND_INVALID", "MCP elicitation action is invalid"));
            }
            if action == "accept" && payload.get("mode").and_then(Value::as_str) != Some("url") {
                let schema = payload.get("requestedSchema").ok_or((
                    "UPSTREAM_PROTOCOL_INVALID",
                    "MCP elicitation request lacks a schema",
                ))?;
                let content = content
                    .as_ref()
                    .ok_or(("COMMAND_INVALID", "accepted MCP elicitation needs content"))?;
                validate_mcp_content(schema, content)?;
            } else if action != "accept" && content.is_some() {
                return Err((
                    "COMMAND_INVALID",
                    "declined MCP elicitation must not contain content",
                ));
            }
            Ok((
                json!({"action":action,"content":content}),
                "mcp_elicitation",
            ))
        }
        _ => Err((
            "COMMAND_INVALID",
            "request action does not match the pending request type",
        )),
    }
}

fn validate_mcp_content(
    schema: &Value,
    content: &Value,
) -> std::result::Result<(), (&'static str, &'static str)> {
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err((
            "UPSTREAM_PROTOCOL_INVALID",
            "MCP elicitation schema is not an object schema",
        ));
    }
    let object = content.as_object().ok_or((
        "COMMAND_INVALID",
        "MCP elicitation content must be an object",
    ))?;
    let properties = schema.get("properties").and_then(Value::as_object).ok_or((
        "UPSTREAM_PROTOCOL_INVALID",
        "MCP elicitation schema is invalid",
    ))?;
    if object.keys().any(|key| !properties.contains_key(key)) {
        return Err((
            "COMMAND_INVALID",
            "MCP elicitation content has an unknown field",
        ));
    }
    if schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|required| {
            required
                .iter()
                .filter_map(Value::as_str)
                .any(|key| !object.contains_key(key))
        })
    {
        return Err((
            "COMMAND_INVALID",
            "MCP elicitation content lacks a required field",
        ));
    }
    for (key, value) in object {
        let field = &properties[key];
        let valid = match field.get("type").and_then(Value::as_str) {
            Some("string") => value.is_string(),
            Some("number") => value.is_number(),
            Some("boolean") => value.is_boolean(),
            Some("array") => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
            _ => false,
        };
        if !valid {
            return Err(("COMMAND_INVALID", "MCP elicitation content type is invalid"));
        }
        if let Some(text) = value.as_str() {
            let length = text.chars().count() as u64;
            if field
                .get("minLength")
                .and_then(Value::as_u64)
                .is_some_and(|minimum| length < minimum)
                || field
                    .get("maxLength")
                    .and_then(Value::as_u64)
                    .is_some_and(|maximum| length > maximum)
            {
                return Err((
                    "COMMAND_INVALID",
                    "MCP elicitation string length is invalid",
                ));
            }
        }
        if let Some(number) = value.as_f64()
            && (field
                .get("minimum")
                .and_then(Value::as_f64)
                .is_some_and(|minimum| number < minimum)
                || field
                    .get("maximum")
                    .and_then(Value::as_f64)
                    .is_some_and(|maximum| number > maximum))
        {
            return Err((
                "COMMAND_INVALID",
                "MCP elicitation number is outside the allowed range",
            ));
        }
        if let Some(values) = value.as_array()
            && (field
                .get("minItems")
                .and_then(Value::as_u64)
                .is_some_and(|minimum| values.len() < minimum as usize)
                || field
                    .get("maxItems")
                    .and_then(Value::as_u64)
                    .is_some_and(|maximum| values.len() > maximum as usize))
        {
            return Err((
                "COMMAND_INVALID",
                "MCP elicitation selection count is invalid",
            ));
        }
        if let Some(allowed) = mcp_allowed_values(field) {
            let values = value
                .as_array()
                .map(|values| values.iter().collect::<Vec<_>>())
                .unwrap_or_else(|| vec![value]);
            if values.iter().any(|value| !allowed.contains(value)) {
                return Err(("COMMAND_INVALID", "MCP elicitation value is not allowed"));
            }
        }
    }
    Ok(())
}

fn mcp_allowed_values(field: &Value) -> Option<Vec<&Value>> {
    if let Some(values) = field.get("enum").and_then(Value::as_array) {
        return Some(values.iter().collect());
    }
    if let Some(values) = field.get("oneOf").and_then(Value::as_array) {
        return Some(
            values
                .iter()
                .filter_map(|value| value.get("const"))
                .collect(),
        );
    }
    if let Some(values) = field.pointer("/items/enum").and_then(Value::as_array) {
        return Some(values.iter().collect());
    }
    field
        .pointer("/items/anyOf")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.get("const"))
                .collect()
        })
}

#[derive(Debug, Clone)]
pub struct NewGatewayCommand {
    pub command_id: String,
    pub principal_id: String,
    pub capability: String,
    pub idempotency_key: String,
    pub payload_hash: String,
    pub target: GatewayCommandTarget,
    pub input_summary_json: String,
    pub origin: GatewayCommandOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Worker-control and channel origins are reserved closed variants for later slices.
pub enum GatewayCommandOrigin {
    LegacyApi,
    Tui,
    WorkerControl,
    Channel,
}

impl GatewayCommandOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LegacyApi => "legacy_api",
            Self::Tui => "tui",
            Self::WorkerControl => "worker_control",
            Self::Channel => "channel",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayCommandTarget {
    pub source_id: String,
    pub source_epoch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_request_version: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayCommandRecord {
    pub command_id: String,
    pub principal_id: String,
    pub capability: String,
    pub target: GatewayCommandTarget,
    pub state: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<GatewayCommandError>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayCommandError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum ReceiveGatewayCommand {
    Created(GatewayCommandRecord),
    Existing(GatewayCommandRecord),
    Conflict,
}

#[derive(Debug, Clone)]
pub struct GatewayTransition {
    pub command_id: String,
    pub to_state: String,
    pub result_summary_json: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub reason_code: Option<String>,
    pub decision: String,
    pub outcome: String,
}

pub fn transition_allowed(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        (
            "received",
            "authorized" | "rejected" | "failed" | "cancelled"
        ) | (
            "authorized",
            "dispatching" | "rejected" | "failed" | "cancelled"
        ) | (
            "dispatching",
            "accepted_by_source" | "rejected" | "failed" | "cancelled" | "outcome_unknown"
        ) | (
            "accepted_by_source",
            "running" | "completed" | "failed" | "cancelled" | "outcome_unknown"
        ) | (
            "running",
            "completed" | "failed" | "cancelled" | "outcome_unknown"
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_state_machine_rejects_terminal_replay_and_skipped_dispatch() {
        assert!(transition_allowed("received", "authorized"));
        assert!(transition_allowed("authorized", "dispatching"));
        assert!(transition_allowed("dispatching", "outcome_unknown"));
        assert!(transition_allowed("running", "completed"));
        assert!(!transition_allowed("received", "completed"));
        assert!(!transition_allowed("completed", "running"));
        assert!(!transition_allowed("outcome_unknown", "dispatching"));
    }

    #[test]
    fn closed_request_actions_cover_supported_terminal_and_channel_semantics() {
        let (approval, kind) = request_action_response(
            "item/commandExecution/requestApproval",
            &json!({"availableDecisions":["accept","decline"]}),
            &PendingRequestAction::Approval {
                decision: "accept".into(),
            },
        )
        .expect("advertised approval");
        assert_eq!(kind, "approval");
        assert_eq!(approval, json!({"decision":"accept"}));
        assert_eq!(
            request_action_response(
                "item/commandExecution/requestApproval",
                &json!({"availableDecisions":["decline"]}),
                &PendingRequestAction::Approval {
                    decision: "accept".into(),
                },
            )
            .unwrap_err()
            .0,
            "CAPABILITY_UNAVAILABLE"
        );

        let (permissions, kind) = request_action_response(
            "item/permissions/requestApproval",
            &json!({"permissions":{"network":true}}),
            &PendingRequestAction::Permissions {
                grant: true,
                scope: "turn".into(),
                strict_auto_review: Some(false),
            },
        )
        .expect("permission response");
        assert_eq!(kind, "permissions");
        assert_eq!(permissions["permissions"], json!({"network":true}));

        let (answers, kind) = request_action_response(
            "item/tool/requestUserInput",
            &json!({"questions":[{"id":"q1","options":[{"label":"yes"}]}]}),
            &PendingRequestAction::UserInput {
                answers: BTreeMap::from([("q1".into(), vec!["yes".into()])]),
            },
        )
        .expect("question response");
        assert_eq!(kind, "user_input");
        assert_eq!(answers["answers"]["q1"]["answers"], json!(["yes"]));

        let schema = json!({
            "type":"object",
            "properties":{
                "name":{"type":"string","minLength":1,"maxLength":8},
                "choices":{"type":"array","items":{"enum":["a","b"]},"minItems":1,"maxItems":2}
            },
            "required":["name"]
        });
        let (elicitation, kind) = request_action_response(
            "mcpServer/elicitation/request",
            &json!({"mode":"form","requestedSchema":schema}),
            &PendingRequestAction::McpElicitation {
                action: "accept".into(),
                content: Some(json!({"name":"fixture","choices":["a"]})),
            },
        )
        .expect("schema-valid MCP elicitation");
        assert_eq!(kind, "mcp_elicitation");
        assert_eq!(elicitation["action"], "accept");
        assert_eq!(
            request_action_response(
                "mcpServer/elicitation/request",
                &json!({"mode":"form","requestedSchema":schema}),
                &PendingRequestAction::McpElicitation {
                    action: "accept".into(),
                    content: Some(json!({"name":"fixture","choices":["forged"]})),
                },
            )
            .unwrap_err()
            .0,
            "COMMAND_INVALID"
        );
    }
}
