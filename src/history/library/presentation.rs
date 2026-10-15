//! Pure reading projection shared by cached metadata and source windows.
//! Callers sanitize source records before invoking these functions.
use super::model::{Entry, ExchangePreview};
use serde_json::{Value, json};
#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;

// These are closed, leading wrappers, not a keyword/XML heuristic. Ordinary
// prose, quoted examples, malformed or nested wrappers remain conversation.
fn closed_block<'a>(input: &'a str, tag: &str) -> Option<(&'a str, &'a str)> {
    let opening = format!("<{tag}>");
    let closing = format!("</{tag}>");
    let body = input.strip_prefix(&opening)?;
    let end = body.find(&closing)?;
    let inside = &body[..end];
    if inside.contains(&opening) {
        return None;
    }
    Some((inside, body[end + closing.len()..].trim_start()))
}

fn environment_body(body: &str) -> bool {
    let mut remaining = body.trim();
    let mut seen = std::collections::HashSet::new();
    while !remaining.is_empty() {
        let next = [
            "cwd",
            "shell",
            "current_date",
            "timezone",
            "filesystem",
            "subagents",
        ]
        .iter()
        .find_map(|tag| closed_block(remaining, tag).map(|(value, tail)| (*tag, value, tail)));
        let Some((tag, value, tail)) = next else {
            return false;
        };
        let valid = match tag {
            "filesystem" => filesystem_body(value),
            "subagents" => subagents_body(value),
            _ => plain_value(value),
        };
        if !seen.insert(tag) || !valid {
            return false;
        }
        remaining = tail;
    }
    seen.contains("cwd") && seen.contains("shell")
}

fn plain_value(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains(['<', '>'])
}

// Recognize only the observed nested environment vocabulary. These small
// parsers reject unknown attributes, child elements, duplicate containers,
// unclosed tags and free text between elements; they are not general XML.
fn filesystem_body(body: &str) -> bool {
    let Some((roots, rest)) = closed_block(body.trim(), "workspace_roots") else {
        return false;
    };
    let mut roots = roots.trim();
    let mut root_count = 0;
    while !roots.is_empty() {
        let Some((path, rest)) = closed_block(roots, "root") else {
            return false;
        };
        if !plain_value(path) {
            return false;
        }
        root_count += 1;
        roots = rest;
    }
    let Some(profile) = rest.strip_prefix("<permission_profile type=\"managed\">") else {
        return false;
    };
    let Some(profile) = profile.trim_end().strip_suffix("</permission_profile>") else {
        return false;
    };
    let Some(entries) = profile
        .trim()
        .strip_prefix("<file_system type=\"restricted\">")
    else {
        return false;
    };
    let Some(mut entries) = entries
        .trim_end()
        .strip_suffix("</file_system>")
        .map(str::trim)
    else {
        return false;
    };
    while !entries.is_empty() {
        let Some(body) = entries
            .strip_prefix("<entry access=\"read\">")
            .or_else(|| entries.strip_prefix("<entry access=\"write\">"))
        else {
            return false;
        };
        let Some((inside, rest)) = body.split_once("</entry>") else {
            return false;
        };
        let value =
            closed_block(inside.trim(), "path").or_else(|| closed_block(inside.trim(), "special"));
        if !value.is_some_and(|(value, rest)| plain_value(value) && rest.is_empty()) {
            return false;
        }
        entries = rest.trim_start();
    }
    root_count > 0
}

fn subagents_body(body: &str) -> bool {
    let mut rest = body.trim();
    while !rest.is_empty() {
        let Some(agent) = rest.strip_prefix("<agent name=\"") else {
            return false;
        };
        let Some((name, tail)) = agent.split_once('"') else {
            return false;
        };
        if !name.starts_with('/') || !plain_value(name) {
            return false;
        }
        let Some(tail) = tail.trim_start().strip_prefix("/>") else {
            return false;
        };
        rest = tail.trim_start();
    }
    true
}

fn without_context(input: &str) -> &str {
    let mut remaining = input.trim();
    loop {
        let tail = if let Some(header) = remaining.strip_prefix("# AGENTS.md instructions for ") {
            let Some((path, body)) = header.split_once('\n') else {
                break;
            };
            if !path.trim().starts_with('/') {
                break;
            }
            closed_block(body.trim_start(), "INSTRUCTIONS").map(|(_, tail)| tail)
        } else if let Some((body, tail)) = closed_block(remaining, "environment_context") {
            // This is the observed Codex environment envelope, not arbitrary XML.
            if environment_body(body) {
                Some(tail)
            } else {
                None
            }
        } else if let Some((body, tail)) = closed_block(remaining, "external_codex_apps_open_page")
        {
            // Only the complete no-page notification observed in the UI is
            // recognized; richer or future payloads require separate evidence.
            serde_json::from_str::<Value>(body)
                .ok()
                .filter(|v| {
                    *v == json!({"page_id":null})
                        && body
                            .chars()
                            .filter(|c| !c.is_ascii_whitespace())
                            .collect::<String>()
                            == "{\"page_id\":null}"
                })
                .map(|_| tail)
        } else {
            None
        };
        let Some(tail) = tail else { break };
        remaining = tail;
    }
    remaining
}

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
