use super::super::fs::Identity;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Size {
    pub bytes: Option<u64>,
    pub known_bytes: u64,
    pub status: String,
    pub measured_at: String,
}
impl Size {
    pub(super) fn new(bytes: u64, complete: bool) -> Self {
        Self {
            bytes: complete.then_some(bytes),
            known_bytes: bytes,
            status: if complete { "complete" } else { "partial" }.into(),
            measured_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageRow {
    pub run_epoch: Uuid,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub state: String,
    pub size: Size,
    pub manual_eligible: bool,
    pub reason: Option<String>,
    pub retention_reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Candidate {
    pub row: UsageRow,
    pub identity: Identity,
    pub lock_identity: Identity,
    pub meta_digest: String,
    pub contents_digest: String,
    pub file_count: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CachedRow {
    pub workspace_id: String,
    pub candidate: Candidate,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewItem {
    pub run_epoch: Uuid,
    pub eligible: bool,
    pub reason: Option<String>,
    pub run: Option<UsageRow>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub preview_id: Uuid,
    pub status: String,
    pub config_revision: Option<String>,
    pub expires_at: String,
    pub mode: String,
    pub executable: bool,
    pub items: Vec<PreviewItem>,
    pub scan_complete: bool,
    pub error: Option<String>,
    pub skipped_counts: std::collections::BTreeMap<String, u64>,
    #[serde(skip)]
    pub(super) candidates: Vec<Candidate>,
    #[serde(skip)]
    pub(super) root_identity: Option<Identity>,
}
#[derive(Clone, Debug)]
pub enum PreviewMode {
    Manual(Vec<Uuid>),
    Retention(u32),
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub scan_id: Uuid,
    pub state: String,
    pub measured_at: String,
    pub run_count: u64,
    pub history_bytes: u64,
    pub active_bytes: u64,
    pub unknown_runs: u64,
    pub unverified_entries: u64,
    pub examined_entries: u64,
    pub runs: Vec<UsageRow>,
    pub next_cursor: Option<String>,
    pub pending_cleanup: Size,
    pub shared_management: Size,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Busy,
    Unavailable,
    NotFound,
    Invalid,
    Stale,
    Disabled,
}
