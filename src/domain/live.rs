use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::classify::summary_text;

/// Validates the recoverable JSON-RPC envelope shape shared by the legacy
/// live transport and the Session Kernel proxy. Unknown methods and fields are
/// intentionally accepted so callers can preserve and forward them raw-first.
pub fn validate_app_server_envelope(envelope: &Value) -> Result<()> {
    let object = envelope.as_object().ok_or_else(|| {
        anyhow::anyhow!("incompatible protocol: JSON-RPC envelope is not an object")
    })?;
    if let Some(method) = object.get("method") {
        if !method.is_string() {
            bail!("incompatible protocol: JSON-RPC method is not a string");
        }
        if let Some(params) = object.get("params")
            && !params.is_object()
            && !params.is_null()
        {
            bail!("incompatible protocol: JSON-RPC params is not an object");
        }
        return Ok(());
    }
    if !object.contains_key("id")
        || (!object.contains_key("result") && !object.contains_key("error"))
    {
        bail!("incompatible protocol: response lacks id and result/error");
    }
    Ok(())
}

pub fn classify_live_item(method: &str, item: Option<&Value>) -> Option<String> {
    if method.contains("requestApproval") {
        return Some("approval".into());
    }
    if method.contains("requestUserInput") || method.contains("elicitation/request") {
        return Some("user_question".into());
    }
    let kind = item
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        .or_else(|| {
            method
                .strip_prefix("item/")
                .and_then(|rest| rest.split('/').next())
        })?;
    Some(
        match kind {
            "userMessage" => "user_message",
            "agentMessage" => "agent_message",
            "reasoning" => "reasoning",
            "commandExecution" => "command_execution",
            "fileChange" => "file_change",
            "mcpToolCall" => "mcp_tool_call",
            "collabAgentToolCall" => "sub_agent",
            "webSearch" => "web_search",
            "imageGeneration" => "image_generation",
            "plan" => "plan",
            "error" => "error",
            other => other,
        }
        .to_string(),
    )
}

pub fn live_summary(params: &Value, item: Option<&Value>) -> Option<String> {
    params
        .get("delta")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            item.and_then(|item| summary_text(&json!({"type":"response_item","payload":item})))
        })
}
