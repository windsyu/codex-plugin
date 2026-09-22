use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ThreadQuery;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadCursor {
    pub(super) endpoint: String,
    pub(super) query_fingerprint: String,
    pub(super) as_of_event_seq: i64,
    pub(super) last_recency_at_ms: i64,
    pub(super) last_thread_key: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PageCursor {
    pub(super) endpoint: String,
    pub(super) query_fingerprint: String,
    pub(super) as_of_event_seq: i64,
    pub(super) last_sort: i64,
    pub(super) last_key: String,
    pub(super) last_secondary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct RequestKey {
    pub(super) source_id: String,
    pub(super) source_epoch: String,
    pub(super) request_id: String,
}

pub(super) enum CursorFailure {
    Invalid(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for CursorFailure {
    fn from(value: anyhow::Error) -> Self {
        Self::Internal(value)
    }
}

pub(super) fn decode_bound_page_cursor(
    encoded: Option<&str>,
    endpoint: &str,
    fingerprint: &str,
    token: &str,
) -> Result<Option<PageCursor>, CursorFailure> {
    let Some(encoded) = encoded else {
        return Ok(None);
    };
    let cursor = decode_page_cursor(encoded, token)
        .map_err(|error| CursorFailure::Invalid(error.to_string()))?;
    if cursor.endpoint != endpoint || cursor.query_fingerprint != fingerprint {
        return Err(CursorFailure::Invalid(
            "cursor does not match the current query".into(),
        ));
    }
    Ok(Some(cursor))
}

pub(super) fn query_fingerprint(value: &Value) -> String {
    blake3::hash(value.to_string().as_bytes())
        .to_hex()
        .to_string()
}

pub(super) fn thread_query_fingerprint(query: &ThreadQuery) -> String {
    let canonical = json!({
        "sourceId":query.source_id,"project":query.project,"runtimeStatus":query.runtime_status,
        "captureCompleteness":query.capture_completeness,"archived":query.archived,"q":query.q,
        "sort":query.sort.as_deref().unwrap_or("recency_desc")
    });
    query_fingerprint(&canonical)
}

fn cursor_key(token: &str) -> Result<[u8; 32]> {
    URL_SAFE_NO_PAD
        .decode(token)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("bearer token must decode to 32 bytes"))
}

pub(super) fn encode_cursor(cursor: &ThreadCursor, token: &str) -> Result<String> {
    encode(cursor, token)
}

pub(super) fn decode_cursor(value: &str, token: &str) -> Result<ThreadCursor> {
    decode(value, token)
}

pub(super) fn encode_page_cursor(cursor: &PageCursor, token: &str) -> Result<String> {
    encode(cursor, token)
}

pub(super) fn encode_request_key(request: &RequestKey, token: &str) -> Result<String> {
    encode(request, token)
}

fn decode_page_cursor(value: &str, token: &str) -> Result<PageCursor> {
    decode(value, token)
}

fn encode<T: Serialize>(cursor: &T, token: &str) -> Result<String> {
    let payload = serde_json::to_vec(cursor)?;
    let signature = blake3::keyed_hash(&cursor_key(token)?, &payload);
    Ok(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(payload),
        URL_SAFE_NO_PAD.encode(signature.as_bytes())
    ))
}

fn decode<T: for<'de> Deserialize<'de>>(value: &str, token: &str) -> Result<T> {
    let (payload, signature) = value
        .split_once('.')
        .ok_or_else(|| anyhow::anyhow!("cursor has an invalid envelope"))?;
    let payload = URL_SAFE_NO_PAD.decode(payload)?;
    let signature = URL_SAFE_NO_PAD.decode(signature)?;
    let expected = blake3::keyed_hash(&cursor_key(token)?, &payload);
    if !constant_time_eq(&signature, expected.as_bytes()) {
        anyhow::bail!("cursor signature is invalid");
    }
    Ok(serde_json::from_slice(&payload)?)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
