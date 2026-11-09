//! Pure reading projection shared by cached metadata and source windows.
//! Callers sanitize source records before invoking these functions.
use super::model::{Entry, ExchangePreview};
use serde_json::{Value, json};
#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;

use crate::context_envelope::without_context;

fn compact(input: &str, limit: usize) -> String {
    input
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

pub(super) fn stored_title(value: &str, key: &[u8; 32]) -> Option<String> {
    let safe = super::model::sanitize(&json!({"text":value}), key);
    let value = safe["text"].as_str()?.trim();
    // A stored instruction dump is not an authored title. Never promote a
    // prefix stripped from a stored name into a supposedly reliable title.
    if value.is_empty()
        || without_context(value) != value
        || matches!(value, "未命名会话" | "旧版会话" | "工作台运行")
    {
        return None;
    }
    Some(compact(value, 120))
}

pub(super) fn observe(entry: &mut Entry, record: &Value) -> bool {
    let view = normalize(record);
    observe_view(entry, &view)
}

pub(super) fn observe_view(entry: &mut Entry, view: &Value) -> bool {
    if view["presentation"] != "conversation" {
        return false;
    }
    let Some(role @ ("user" | "assistant")) = view["role"].as_str() else {
        return false;
    };
    let text = compact(view["text"].as_str().unwrap_or(""), 240);
    if text.is_empty() {
        return false;
    }
    if role == "user" && entry.title == "未命名会话" {
        entry.title = text.chars().take(120).collect();
    }
    entry.preview = Some(ExchangePreview {
        text,
        role: role.into(),
    });
    true
}

fn text(value: &Value) -> String {
    for field in [
        "text",
        "summaryText",
        "message",
        "output",
        "arguments",
        "command",
    ] {
        if let Some(s) = value[field].as_str() {
            return s.into();
        }
    }
    if let Some(text) = value["content"].as_str() {
        return text.into();
    }
    for field in ["content", "summary"] {
        if let Some(values) = value[field].as_array() {
            let s = values
                .iter()
                .filter_map(|v| {
                    v["text"]
                        .as_str()
                        .or(v["input_text"].as_str())
                        .or(v["output_text"].as_str())
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !s.is_empty() {
                return s;
            }
        }
    }
    String::new()
}
pub(super) fn normalize(record: &Value) -> Value {
    let mut kind = record["kind"].as_str().unwrap_or("unknown").to_owned();
    let raw = &record["raw"];
    let mut content = text(record);
    let mut role = None;
    let mut label = String::new();
    if kind == "observer_item" {
        kind = raw["itemType"].as_str().unwrap_or("unknown").into();
        content = text(raw);
        if content.is_empty() {
            content = text(&raw["raw"]["payload"]);
        }
    } else if kind == "items" {
        kind = raw["kind"].as_str().unwrap_or("unknown").into();
        role = raw["author"]["role"].as_str();
        content = text(raw);
    }
    if matches!(kind.as_str(), "user_message" | "userMessage") {
        role = Some("user");
    }
    if matches!(kind.as_str(), "agent_message" | "agentMessage") {
        role = Some("assistant");
    }
    let payload = if raw["payload"].is_object() {
        &raw["payload"]
    } else {
        raw
    };
    // Native summaryText may already be truncated before the wrapper closes.
    // Identify the complete, bounded source message before display truncation.
    if role.is_some() && raw["payload"].is_object() {
        let source_text = if payload["type"] == "item_completed" {
            text(&payload["item"])
        } else {
            text(payload)
        };
        if !source_text.is_empty() {
            content = source_text;
        }
    }
    if content.is_empty() {
        content = text(payload);
    }
    if matches!(kind.as_str(), "tool_call" | "toolCall") {
        if let Some(arguments) = payload["arguments"].as_str().or(raw["arguments"].as_str()) {
            content = serde_json::from_str::<Value>(arguments)
                .ok()
                .and_then(|v| {
                    v["cmd"]
                        .as_str()
                        .or(v["command"].as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| arguments.into());
        }
    } else if kind == "tool_output"
        && let Some(output) = payload["output"].as_str()
    {
        content = output.into();
    }
    if let Some(name) = payload["name"].as_str() {
        label = name.into();
    }
    let presentation = if matches!(kind.as_str(), "error" | "history_gap")
        || (kind == "notice" && raw["code"] == "capture_gap")
    {
        "attention"
    } else if matches!(
        kind.as_str(),
        "context" | "observer_context" | "observer_instructions"
    ) || record["presentation"] == "context"
    {
        "context"
    } else if record["presentation"] == "activity"
        || (role == Some("assistant")
            && (payload["channel"] == "analysis" || raw["raw"]["payload"]["channel"] == "analysis"))
    {
        "activity"
    } else if matches!(role, Some("user" | "assistant"))
        && matches!(
            kind.as_str(),
            "message" | "user_message" | "userMessage" | "agent_message" | "agentMessage"
        )
    {
        if role == Some("user") {
            let visible = without_context(&content);
            if visible.is_empty() && !content.trim().is_empty() {
                "context"
            } else {
                content = visible.into();
                "conversation"
            }
        } else {
            "conversation"
        }
    } else {
        "activity"
    };
    let truncated = content.chars().count() > 12000
        || record["detailsOmitted"] == true
        || raw["truncated"] == true;
    let content = content.chars().take(12000).collect::<String>();
    json!({"kind":kind,"role":role,"presentation":presentation,"text":content,"label":label,"status":payload["status"],"truncated":truncated,"offset":record["offset"],"issues":record["issues"],"model":raw["author"]["requestedModel"],"usage":if kind=="responses"{raw["usage"].clone()}else{Value::Null}})
}
