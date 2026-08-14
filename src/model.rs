use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct NormalizedEvent {
    pub event_id: String,
    pub source_id: String,
    pub store_source_id: String,
    pub epoch_id: String,
    pub source_seq: i64,
    pub dedupe_key: String,
    pub observed_at_ms: i64,
    pub event_at_ms: Option<i64>,
    pub thread_key: String,
    pub codex_thread_id: String,
    pub turn_id: Option<String>,
    pub item_id: Option<String>,
    pub request_id: Option<String>,
    pub blob_id: Option<String>,
    pub method: String,
    pub phase: String,
    pub durability: String,
    pub projectable: bool,
    pub source_fingerprint: String,
    pub stored_raw_hash: String,
    pub raw_json: String,
    pub redaction_json: String,
    pub decode_status: String,
    pub decode_error: Option<String>,
    pub top_type: String,
    pub item_type: Option<String>,
    pub item_status: Option<String>,
    pub summary_text: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Default, Clone)]
pub struct Checkpoint {
    pub byte_offset: u64,
    pub ordinal: u64,
    pub epoch_id: Option<String>,
    pub file_identity: Option<String>,
    pub current_turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEnvelope<T> {
    pub api_version: &'static str,
    pub as_of_event_seq: i64,
    pub data: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl<T> ApiEnvelope<T> {
    pub fn new(as_of_event_seq: i64, data: T) -> Self {
        Self {
            api_version: "v1",
            as_of_event_seq,
            data,
            next_cursor: None,
        }
    }

    pub fn with_cursor(as_of_event_seq: i64, data: T, next_cursor: Option<String>) -> Self {
        Self {
            api_version: "v1",
            as_of_event_seq,
            data,
            next_cursor,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub files_scanned: usize,
    pub events_inserted: usize,
    pub events_deduplicated: usize,
    pub decode_errors: usize,
    pub sources_degraded: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionReport {
    pub applied: bool,
    pub cutoff_at_ms: i64,
    pub candidate_raw_events: usize,
    pub deleted_raw_events: usize,
    pub candidate_blobs: usize,
    pub deleted_blobs: usize,
    pub low_watermark_before: i64,
    pub low_watermark_after: i64,
    pub dedupe_tombstones_retained: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    pub thread_key: String,
    pub output: String,
    pub turns: usize,
    pub items: usize,
    pub raw_events: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeReport {
    pub thread_key: String,
    pub deleted_turns: usize,
    pub deleted_items: usize,
    pub deleted_raw_events: usize,
    pub deleted_blobs: usize,
    pub suppression_tombstone: bool,
    pub audit_id: String,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub status: String,
    pub database: String,
    pub sources: Vec<DoctorSource>,
}

#[derive(Debug, Serialize)]
pub struct DoctorSource {
    pub name: String,
    pub path: String,
    pub status: String,
}
