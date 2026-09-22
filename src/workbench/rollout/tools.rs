//! Closed native command evidence, verified against CLI 0.154.0 rollouts.
use super::*;
use crate::workbench::decode::tool::{PREVIEW_BYTES, identifier};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCommandStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeCommand {
    pub key: UserKey,
    pub source: UserSource,
    pub status: NativeCommandStatus,
    pub process_id: Option<String>,
    pub command_source: String,
    pub command: Vec<String>,
    pub cwd: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<f64>,
    pub truncated: bool,
    pub omitted: bool,
    #[serde(skip)]
    pub(super) fingerprint: blake3::Hash,
}
impl NativeCommand {
    pub(crate) fn bytes(&self) -> usize {
        self.output.len()
            + self.cwd.len()
            + self.command.iter().map(String::len).sum::<usize>()
            + 1024
    }
    pub(crate) fn same_evidence(&self, other: &Self) -> bool {
        self.key == other.key && self.fingerprint == other.fingerprint
    }
}

pub(super) fn command(
    value: &Value,
    thread: Uuid,
    source: Uuid,
    offset: u64,
    policy: &Arc<RedactionPolicy>,
) -> Result<Option<NativeCommand>, UserIssue> {
    let event = &value["payload"];
    if value["type"] != "event_msg"
        || !matches!(
            event["type"].as_str(),
            Some("item_started" | "item_completed")
        )
        || event["item"]["type"] != "CommandExecution"
    {
        return Ok(None);
    }
    if event["thread_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        != Some(thread)
    {
        return Err(UserIssue::IdentityConflict);
    }
    let item = &event["item"];
    let turn = identifier(&event["turn_id"], policy).ok_or(UserIssue::MissingIdentity)?;
    let id = identifier(&item["id"], policy).ok_or(UserIssue::MissingIdentity)?;
    let status = match (event["type"].as_str(), item["status"].as_str()) {
        (Some("item_started"), Some("in_progress")) => NativeCommandStatus::InProgress,
        (Some("item_completed"), Some("completed")) => NativeCommandStatus::Completed,
        (Some("item_completed"), Some("failed")) => NativeCommandStatus::Failed,
        (Some("item_completed"), Some("declined")) => NativeCommandStatus::Declined,
        _ => return Err(UserIssue::UnsupportedToolEvidence),
    };
    let command_source = match item["source"].as_str() {
        Some(
            source @ ("agent" | "user_shell" | "unified_exec_startup" | "unified_exec_interaction"),
        ) => source.to_owned(),
        _ => return Err(UserIssue::UnsupportedToolEvidence),
    };
    let exit_code = if item["exit_code"].is_null() {
        None
    } else {
        Some(
            item["exit_code"]
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or(UserIssue::InvalidLine)?,
        )
    };
    if (status == NativeCommandStatus::Completed && exit_code.is_some_and(|code| code != 0))
        || (status == NativeCommandStatus::Failed && exit_code == Some(0))
    {
        return Err(UserIssue::IdentityConflict);
    }
    let duration_ms = if item["duration"].is_null() {
        None
    } else {
        let secs = item["duration"]["secs"]
            .as_u64()
            .ok_or(UserIssue::InvalidLine)?;
        let nanos = item["duration"]["nanos"]
            .as_u64()
            .filter(|n| *n < 1_000_000_000)
            .ok_or(UserIssue::InvalidLine)?;
        Some(secs as f64 * 1000.0 + nanos as f64 / 1_000_000.0)
    };
    let raw = item["aggregated_output"].as_str();
    let safe = policy.scrub_tool(raw.unwrap_or_default());
    let mut truncated = safe.as_str().len() > PREVIEW_BYTES
        || crate::workbench::decode::output::has_truncation_marker(raw.unwrap_or_default());
    let mut omitted = status != NativeCommandStatus::InProgress && raw.is_none();
    let mut command_bytes = 0;
    let mut command = Vec::new();
    if let Some(argv) = item["command"].as_array() {
        for arg in argv.iter().take(128) {
            if let Some(arg) = arg.as_str() {
                let safe = policy.scrub_tool(arg);
                let kept = crate::workbench::live::prefix(
                    safe.as_str(),
                    PREVIEW_BYTES.saturating_sub(command_bytes),
                );
                truncated |= kept.len() < safe.as_str().len();
                command_bytes += kept.len();
                command.push(kept.to_owned());
            } else {
                omitted = true;
            }
        }
        truncated |= argv.len() > 128;
    } else {
        omitted = true;
    }
    let process_id = if item["process_id"].is_null() {
        None
    } else {
        Some(identifier(&item["process_id"], policy).ok_or(UserIssue::MissingIdentity)?)
    };
    let cwd = policy.scrub_tool(item["cwd"].as_str().unwrap_or_default());
    truncated |= cwd.as_str().len() > 4096;
    Ok(Some(NativeCommand {
        key: UserKey {
            codex_thread_id: thread,
            codex_turn_id: turn,
            native_item_id: id,
        },
        source: UserSource {
            source_ref: source,
            byte_offset: offset,
            ordinal: value["ordinal"].as_u64(),
        },
        status,
        process_id,
        command_source,
        command,
        cwd: cwd.bounded(4096).as_str().to_owned(),
        output: safe.bounded(PREVIEW_BYTES).as_str().to_owned(),
        exit_code,
        duration_ms,
        truncated,
        omitted,
        fingerprint: blake3::hash(&serde_json::to_vec(item).unwrap_or_default()),
    }))
}
