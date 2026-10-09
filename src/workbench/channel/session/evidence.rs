//! Minimal native lifecycle decoder. Never interprets model/SSE completion.
use super::{Evidence, Failure, NativeStatus};
use serde_json::Value;
use uuid::Uuid;

fn id(value: &Value) -> Result<&str, Failure> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
        .ok_or(Failure::InvalidEvidence)
}
pub(super) fn local_input(value: &Value, target: Uuid) -> bool {
    matches!(
        value["method"].as_str(),
        Some("turn/start" | "turn/steer" | "thread/queue/add")
    ) && value["params"]["threadId"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        == Some(target)
}
pub(super) fn parse(value: &Value, target: Uuid) -> Result<Option<Evidence<'_>>, Failure> {
    let Some(method) = value["method"].as_str() else {
        return Ok(None);
    };
    // Requests (including approvals) are never lifecycle notifications.
    if value.get("id").is_some() {
        return Ok(None);
    }
    if !matches!(
        method,
        "turn/started" | "turn/completed" | "item/started" | "item/completed"
    ) {
        return Ok(None);
    }
    let p = &value["params"];
    let thread = id(&p["threadId"])?;
    let thread_uuid = Uuid::parse_str(thread).map_err(|_| Failure::InvalidEvidence)?;
    if thread_uuid != target {
        return Ok(None);
    }
    if matches!(method, "turn/started" | "turn/completed") {
        let turn = id(&p["turn"]["id"])?;
        let status = match p["turn"]["status"].as_str() {
            Some("inProgress") if method == "turn/started" => {
                return Ok(Some(Evidence::TurnStarted { thread, turn }));
            }
            Some("completed") if method == "turn/completed" => NativeStatus::Completed,
            Some("failed") if method == "turn/completed" => NativeStatus::Failed,
            Some("interrupted") if method == "turn/completed" => NativeStatus::Interrupted,
            _ => return Err(Failure::InvalidEvidence),
        };
        return Ok(Some(Evidence::TurnCompleted {
            thread,
            turn,
            status,
        }));
    }
    let turn = id(&p["turnId"])?;
    let item = &p["item"];
    let item_id = id(&item["id"])?;
    match item["type"].as_str() {
        Some("userMessage") => {
            let client = match item.get("clientId") {
                None | Some(Value::Null) => None,
                Some(value) => Some(id(value)?),
            };
            if !item["content"].is_array() {
                return Err(Failure::InvalidEvidence);
            }
            Ok(Some(if method == "item/started" {
                Evidence::UserStarted {
                    thread,
                    turn,
                    item: item_id,
                    client,
                }
            } else {
                Evidence::UserCompleted {
                    thread,
                    turn,
                    item: item_id,
                    client,
                }
            }))
        }
        Some("agentMessage") => {
            let phase = item["phase"].as_str().ok_or(Failure::InvalidEvidence)?;
            if !matches!(phase, "commentary" | "final_answer") {
                return Err(Failure::InvalidEvidence);
            }
            let text = item["text"].as_str().ok_or(Failure::InvalidEvidence)?;
            if method == "item/completed" && phase == "final_answer" {
                Ok(Some(Evidence::FinalAnswer {
                    thread,
                    turn,
                    item: item_id,
                    text,
                }))
            } else {
                Ok(None)
            }
        }
        // Known non-user item variants contribute no export text.
        Some(
            "hookPrompt"
            | "functionCallOutput"
            | "plan"
            | "reasoning"
            | "commandExecution"
            | "fileChange"
            | "mcpToolCall"
            | "dynamicToolCall"
            | "collabAgentToolCall"
            | "subAgentActivity"
            | "webSearch"
            | "imageView"
            | "sleep"
            | "imageGeneration"
            | "enteredReviewMode"
            | "exitedReviewMode"
            | "contextCompaction",
        ) => Ok(None),
        _ => Err(Failure::InvalidEvidence),
    }
}
