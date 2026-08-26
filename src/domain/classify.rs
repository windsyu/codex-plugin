use serde_json::Value;

pub fn classify_item(raw: &Value) -> Option<String> {
    let top = raw.get("type")?.as_str()?;
    let payload = raw.get("payload")?;
    let kind = payload.get("type").and_then(Value::as_str).unwrap_or(top);
    match top {
        "response_item" => Some(
            match kind {
                "message" => match payload.get("role").and_then(Value::as_str) {
                    Some("user") => "user_message",
                    _ => "agent_message",
                },
                "reasoning" => "reasoning",
                "local_shell_call" => "command_execution",
                "function_call" | "custom_tool_call" | "tool_search_call" => "tool_call",
                "function_call_output" | "custom_tool_call_output" => "tool_output",
                other => other,
            }
            .to_string(),
        ),
        "compacted" => Some("reasoning".into()),
        "inter_agent_communication" | "inter_agent_communication_metadata" => {
            Some("sub_agent".into())
        }
        "event_msg" => match kind {
            "user_message" => Some("user_message".into()),
            "agent_message" => Some("agent_message".into()),
            "agent_reasoning" => Some("reasoning".into()),
            "exec_command_begin" | "exec_command_end" | "exec_command_output_delta" => {
                Some("command_execution".into())
            }
            "mcp_tool_call_begin" | "mcp_tool_call_end" => Some("mcp_tool_call".into()),
            "turn_diff" | "patch_apply_begin" | "patch_apply_end" => Some("file_change".into()),
            "plan_update" => Some("plan".into()),
            "error" | "warning" | "stream_error" => Some("error".into()),
            "token_count" => Some("usage".into()),
            _ => None,
        },
        other if !matches!(other, "session_meta" | "turn_context" | "world_state") => {
            Some("unknown".into())
        }
        _ => None,
    }
}

pub fn summary_text(raw: &Value) -> Option<String> {
    let payload = raw.get("payload")?;
    if let Some(text) = payload
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| payload.get("content").and_then(Value::as_str))
        .or_else(|| payload.get("text").and_then(Value::as_str))
        .or_else(|| payload.get("name").and_then(Value::as_str))
        .or_else(|| payload.get("call_id").and_then(Value::as_str))
    {
        return Some(truncate(text, 16_000));
    }
    for field in ["content", "summary"] {
        if let Some(values) = payload.get(field).and_then(Value::as_array) {
            let joined = values
                .iter()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("input_text").and_then(Value::as_str))
                        .or_else(|| item.get("output_text").and_then(Value::as_str))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !joined.is_empty() {
                return Some(truncate(&joined, 16_000));
            }
        }
    }
    None
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}
