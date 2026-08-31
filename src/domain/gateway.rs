use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct NewGatewayCommand {
    pub command_id: String,
    pub principal_id: String,
    pub capability: String,
    pub idempotency_key: String,
    pub payload_hash: String,
    pub target: GatewayCommandTarget,
    pub input_summary_json: String,
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
}
