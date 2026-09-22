//! Canonical FileChange completion evidence. Paths and file bodies are never read.
use super::*;
use crate::workbench::decode::tool::{PREVIEW_BYTES, identifier};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativePatchStatus {
    Completed,
    Failed,
    Declined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeFileOperation {
    Add,
    Update,
    Delete,
    Unknown,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeChangedFile {
    pub path: String,
    pub operation: NativeFileOperation,
    pub move_to: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeFileChange {
    pub key: UserKey,
    pub source: UserSource,
    pub status: NativePatchStatus,
    pub files: Vec<NativeChangedFile>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub truncated: bool,
    pub omitted: bool,
    #[serde(skip)]
    fingerprint: blake3::Hash,
}
impl NativeFileChange {
    pub(crate) fn bytes(&self) -> usize {
        1024 + self.stdout.as_ref().map_or(0, String::len)
            + self.stderr.as_ref().map_or(0, String::len)
            + self
                .files
                .iter()
                .map(|file| file.path.len() + file.move_to.as_ref().map_or(0, String::len) + 128)
                .sum::<usize>()
    }
    pub(crate) fn same_evidence(&self, other: &Self) -> bool {
        self.key == other.key && self.fingerprint == other.fingerprint
    }
}

pub(super) fn file_change(
    value: &Value,
    thread: Uuid,
    source: Uuid,
    offset: u64,
    policy: &Arc<RedactionPolicy>,
) -> Result<Option<NativeFileChange>, UserIssue> {
    let event = &value["payload"];
    if value["type"] != "event_msg"
        || event["type"] != "item_completed"
        || event["item"]["type"] != "FileChange"
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
    let status = match item["status"].as_str() {
        Some("completed") => NativePatchStatus::Completed,
        Some("failed") => NativePatchStatus::Failed,
        Some("declined") => NativePatchStatus::Declined,
        _ => return Err(UserIssue::UnsupportedToolEvidence),
    };
    let (mut truncated, mut omitted) = (false, false);
    let mut stream = |name: &str| {
        let Some(raw) = item[name].as_str() else {
            omitted = true;
            return None;
        };
        let safe = policy.scrub_tool(raw);
        let kept = crate::workbench::live::prefix(safe.as_str(), PREVIEW_BYTES / 2);
        truncated |= kept.len() < safe.as_str().len()
            || crate::workbench::decode::output::has_truncation_marker(raw);
        Some(kept.to_owned())
    };
    let stdout = stream("stdout");
    let stderr = stream("stderr");
    let mut files = Vec::new();
    let mut path_bytes = 0;
    if let Some(changes) = item["changes"].as_object() {
        truncated |= changes.len() > 64;
        for (path, change) in changes.iter().take(64) {
            let operation = match change["type"].as_str() {
                Some("add") => NativeFileOperation::Add,
                Some("update") => NativeFileOperation::Update,
                Some("delete") => NativeFileOperation::Delete,
                _ => {
                    omitted = true;
                    NativeFileOperation::Unknown
                }
            };
            let mut safe_path = |raw: &str| {
                let safe = policy.scrub_tool(raw);
                let kept = crate::workbench::live::prefix(
                    safe.as_str(),
                    4096.min(PREVIEW_BYTES.saturating_sub(path_bytes)),
                );
                truncated |= kept.len() < safe.as_str().len();
                path_bytes += kept.len();
                kept.to_owned()
            };
            let path = safe_path(path);
            let move_to = change["move_path"].as_str().map(&mut safe_path);
            if !change["move_path"].is_null() && move_to.is_none() {
                omitted = true;
            }
            files.push(NativeChangedFile {
                path,
                operation,
                move_to,
            });
        }
    } else {
        omitted = true;
    }
    Ok(Some(NativeFileChange {
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
        files,
        stdout,
        stderr,
        truncated,
        omitted,
        fingerprint: blake3::hash(&serde_json::to_vec(item).unwrap_or_default()),
    }))
}
