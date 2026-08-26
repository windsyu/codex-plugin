use serde_json::{Value, json};

use super::classify::summary_text;

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
