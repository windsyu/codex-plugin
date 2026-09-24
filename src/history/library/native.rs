//! Streaming rollout reader. Only SessionMeta.id is a native thread identity.
use super::{model::*, *};
use crate::history::{classify, files::BlobRoot, files::signature};
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};
const LINE: usize = 1024 * 1024;
const BODY: usize = 2 * 1024 * 1024;
pub(super) struct NativeFile {
    reader: Box<dyn BufRead>,
    file: std::fs::File,
    signature: [u64; 7],
    relative: String,
    pub document: Document,
    offset: u64,
    line: Vec<u8>,
    discarding: bool,
    bytes: usize,
    identity: bool,
    last_message: Option<(String, String, String)>,
}
impl NativeFile {
    pub fn open(root: &BlobRoot, relative: String, source: &Source) -> std::io::Result<Self> {
        let file = root.read_file(&relative)?;
        let sig = signature(&file.metadata()?);
        let input = file.try_clone()?;
        let reader: Box<dyn BufRead> = if relative.ends_with(".zst") {
            let mut decoder = zstd::stream::read::Decoder::new(input)?;
            decoder.window_log_max(23)?;
            Box::new(BufReader::new(decoder))
        } else {
            Box::new(BufReader::new(input))
        };
        let mut entry = Entry::new(source, &relative);
        entry.coverage.state = "complete_for_source".into();
        Ok(Self {
            reader,
            file,
            signature: sig,
            relative: relative.clone(),
            document: Document {
                entry,
                records: vec![],
                locator: json!({"path":relative,"signature":sig}),
            },
            offset: 0,
            line: vec![],
            discarding: false,
            bytes: 0,
            identity: false,
            last_message: None,
        })
    }
    pub fn checkpoint(&self) -> String {
        digest(&format!("{}:{:?}", self.relative, self.signature))
    }
    // Retains reader, partial JSON line and byte offset between bounded batches.
    pub fn step(
        &mut self,
        source: &Source,
        root: &BlobRoot,
        key: &[u8; 32],
    ) -> std::io::Result<bool> {
        let start = Instant::now();
        let mut consumed = 0;
        while consumed < 4 * 1024 * 1024 && start.elapsed() < Duration::from_millis(100) {
            let input = self.reader.fill_buf()?;
            if input.is_empty() {
                if !self.line.is_empty() || self.discarding {
                    self.document.entry.issue_at("partial_tail", self.offset);
                }
                if signature(&self.file.metadata()?) != self.signature
                    || signature(&root.read_file(&self.relative)?.metadata()?) != self.signature
                {
                    self.document
                        .entry
                        .issue_at("source_changed_during_read", self.offset);
                    self.document.entry.capabilities.resume = false;
                }
                if !self.identity {
                    self.document
                        .entry
                        .issue_at("session_identity_missing", self.offset);
                }
                self.document.entry.source_revision = digest(&format!("{:?}", self.signature));
                return Ok(true);
            }
            let count = input
                .iter()
                .position(|b| *b == b'\n')
                .map_or(input.len(), |i| i + 1);
            let end = input[count - 1] == b'\n';
            if !self.discarding {
                if self.line.len() + count > LINE {
                    self.line.clear();
                    self.discarding = true;
                    self.document.entry.issue_at("line_limit", self.offset);
                } else {
                    self.line.extend_from_slice(&input[..count]);
                }
            }
            self.reader.consume(count);
            consumed += count;
            self.offset += count as u64;
            if end {
                if !self.discarding {
                    let bytes = std::mem::take(&mut self.line);
                    self.parse(&bytes, source, key);
                }
                self.discarding = false;
            }
        }
        Ok(false)
    }
    pub(super) fn parse(&mut self, bytes: &[u8], source: &Source, key: &[u8; 32]) {
        let raw = match serde_json::from_slice::<Value>(bytes) {
            Ok(v) => v,
            Err(_) => {
                self.document
                    .entry
                    .issue_at("invalid_json_line", self.offset);
                return;
            }
        };
        let safe = sanitize(&raw, key);
        let payload = &safe["payload"];
        let top = safe["type"].as_str().unwrap_or("");
        if top == "session_meta" {
            if let Some(id) = payload["id"]
                .as_str()
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
            {
                if self.identity
                    && self.document.entry.native_thread_id.as_deref() != Some(&id.to_string())
                {
                    self.document
                        .entry
                        .issue_at("conflicting_session_identity", self.offset);
                    self.document.entry.capabilities.resume = false;
                    return;
                }
                self.identity = true;
                self.document.entry.entry_id = Entry::new(source, &id.to_string()).entry_id;
                self.document.entry.native_thread_id = Some(id.to_string());
                self.document.entry.capabilities.resume = true;
            }
            self.document
                .entry
                .native_path(payload["cwd"].as_str(), payload["originator"].as_str());
            let agent = &payload["source"]["subagent"]["thread_spawn"];
            let top_parent = payload.get("parent_thread_id").filter(|v| !v.is_null());
            let nested_parent = agent.get("parent_thread_id").filter(|v| !v.is_null());
            let parent =
                if top_parent.is_some() && nested_parent.is_some() && top_parent != nested_parent {
                    self.document
                        .entry
                        .issue_at("conflicting_parent_identity", self.offset);
                    None
                } else {
                    top_parent.or(nested_parent)
                };
            self.document.entry.parent_thread_id = parent
                .and_then(Value::as_str)
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .filter(|id| !id.is_nil())
                .map(|id| id.to_string());
            if parent.is_some() && self.document.entry.parent_thread_id.is_none() {
                self.document
                    .entry
                    .issue_at("invalid_parent_identity", self.offset);
            }
            self.document.entry.is_subagent = !payload["source"]["subagent"].is_null()
                || top_parent.is_some()
                || nested_parent.is_some();
            self.document.entry.parent_entry_id = self
                .document
                .entry
                .parent_thread_id
                .as_ref()
                .map(|id| Entry::new(source, id).entry_id);
            self.document.entry.agent_name = None;
            for key in ["agent_nickname", "agent_role"] {
                let top = payload[key].as_str();
                let nested = agent[key].as_str();
                if top.is_some() && nested.is_some() && top != nested {
                    self.document
                        .entry
                        .issue_at("conflicting_agent_metadata", self.offset);
                } else if self.document.entry.agent_name.is_none() {
                    self.document.entry.agent_name =
                        top.or(nested).map(|s| s.chars().take(120).collect());
                }
            }
            if payload.get("history_base").is_some_and(|v| !v.is_null()) {
                self.document
                    .entry
                    .issue_at("inherited_history_not_loaded", self.offset);
            }
            self.document.entry.recorded_at = payload["timestamp"].as_str().and_then(timestamp);
        } else if top == "turn_context" { /* Context remains a separate record, never a chat message. */
        }
        let mut kind = classify::classify_item(&safe).unwrap_or_else(|| {
            if matches!(top, "session_meta" | "turn_context" | "world_state") {
                "context".into()
            } else {
                "unknown".into()
            }
        });
        let mut summary = classify::summary_text(&safe).unwrap_or_default();
        if top == "response_item" && payload["type"] == "message" {
            kind = match payload["role"].as_str() {
                Some("user") => "user_message",
                Some("assistant") => "agent_message",
                Some("developer" | "system") => "context",
                _ => "unknown",
            }
            .into();
        }
        if top == "event_msg" {
            if payload["type"] == "item_completed" {
                let item = &payload["item"];
                kind = match item["type"].as_str() {
                    Some("UserMessage") => "user_message",
                    Some("AgentMessage") => "agent_message",
                    Some("Reasoning") => "reasoning",
                    _ => "unknown",
                }
                .into();
                summary = classify::summary_text(&serde_json::json!({"payload":item}))
                    .unwrap_or_default();
                if payload["thread_id"]
                    .as_str()
                    .is_some_and(|id| self.document.entry.native_thread_id.as_deref() != Some(id))
                {
                    self.document
                        .entry
                        .issue_at("event_identity_conflict", self.offset);
                    kind = "unknown".into();
                }
            } else if matches!(
                payload["type"].as_str(),
                Some(
                    "task_started"
                        | "task_complete"
                        | "turn_started"
                        | "turn_complete"
                        | "turn_aborted"
                        | "item_started"
                        | "context_compacted"
                        | "session_configured"
                )
            ) {
                kind = "context".into();
            }
        }
        if !matches!(
            kind.as_str(),
            "user_message"
                | "agent_message"
                | "context"
                | "reasoning"
                | "command_execution"
                | "tool_call"
                | "tool_output"
                | "sub_agent"
                | "mcp_tool_call"
                | "file_change"
                | "plan"
                | "error"
                | "usage"
        ) {
            self.document.entry.issue_at("unknown_event", self.offset);
        }

        if matches!(kind.as_str(), "user_message" | "agent_message") {
            // The event and response forms are mirrors. Suppress only the immediately adjacent opposite-form copy, not repeated turns.
            if self
                .last_message
                .as_ref()
                .is_some_and(|(k, t, s)| k == &kind && t != top && s == &digest(&summary))
            {
                self.last_message = None;
                return;
            }
            self.last_message = Some((kind.clone(), top.into(), digest(&summary)));
            if kind == "user_message"
                && self.document.entry.title == "未命名会话"
                && !summary.is_empty()
            {
                self.document.entry.title = summary.chars().take(120).collect();
            }
        } else {
            self.last_message = None;
        }
        if let Some(time) = safe["timestamp"].as_str().and_then(timestamp) {
            self.document.entry.recorded_at = Some(time);
        }
        let mut record = serde_json::json!({"kind":kind,"text":summary,"offset":self.offset,"recordId":self.offset.saturating_sub(bytes.len() as u64),"raw":safe});
        let mut size = serde_json::to_vec(&record).unwrap().len();
        if size > 64 * 1024 {
            record.as_object_mut().unwrap().remove("raw");
            record["detailsOmitted"] = true.into();
            size = serde_json::to_vec(&record).unwrap().len();
            self.document.entry.issue_at("detail_limit", self.offset);
        }
        if self.bytes + size <= BODY {
            self.bytes += size;
            self.document.records.push(record);
        } else {
            self.document
                .entry
                .issue_at("body_cache_limit", self.offset);
        }
    }
}

fn timestamp(value: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value).ok().map(|d| {
        d.with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    })
}

/// A source window is independent of the small catalog preview. Byte positions
/// are in the uncompressed stream; compressed seeking has a time/byte budget.
pub(super) fn window(
    source: &Source,
    locator: &Value,
    entry: &Entry,
    state: &Value,
    limit: usize,
    details: bool,
    key: &[u8; 32],
) -> super::Result<super::window::Window> {
    use std::io::{Read, Seek, SeekFrom};
    let root = BlobRoot::open(source.path()).map_err(|_| "source_unavailable")?;
    let path = locator["path"].as_str().ok_or("locator_unavailable")?;
    let mut file =
        NativeFile::open(&root, path.into(), source).map_err(|_| "source_unavailable")?;
    if json!(file.signature) != locator["signature"]
        || digest(&format!("{:?}", file.signature)) != entry.source_revision
    {
        return Err("source_revision_changed");
    }
    let offset = state["offset"].as_u64().unwrap_or(0);
    let started = Instant::now();
    if path.ends_with(".zst") {
        let mut left = offset;
        let mut buffer = [0; 32 * 1024];
        while left > 0 {
            if started.elapsed() > Duration::from_secs(1) || offset - left > 128 * 1024 * 1024 {
                return Err("source_seek_budget");
            }
            let length = (left as usize).min(buffer.len());
            let n = file
                .reader
                .read(&mut buffer[..length])
                .map_err(|_| "source_read_failed")?;
            if n == 0 {
                return Err("source_revision_changed");
            }
            left -= n as u64;
        }
    } else {
        let mut input = file.file.try_clone().map_err(|_| "source_read_failed")?;
        input
            .seek(SeekFrom::Start(offset))
            .map_err(|_| "source_read_failed")?;
        file.reader = Box::new(BufReader::new(input));
    }
    file.document.entry = entry.clone();
    file.identity = entry.native_thread_id.is_some();
    file.offset = offset;
    file.last_message = serde_json::from_value(state["mirror"].clone())
        .ok()
        .flatten();
    let mut records = vec![];
    let mut next = None;
    let mut consumed = 0;
    for _ in 0..limit {
        let start = file.offset;
        let mirror = file.last_message.clone();
        let mut bytes = vec![];
        file.reader
            .by_ref()
            .take(LINE as u64 + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(|_| "source_read_failed")?;
        if bytes.is_empty() {
            break;
        }
        file.offset += bytes.len() as u64;
        consumed += bytes.len();
        if bytes.len() > LINE {
            // Consume the rest without allocating the oversized line.
            loop {
                let input = file.reader.fill_buf().map_err(|_| "source_read_failed")?;
                if input.is_empty() {
                    break;
                }
                let n = input
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(input.len(), |i| i + 1);
                let end = input[n - 1] == b'\n';
                file.reader.consume(n);
                file.offset += n as u64;
                consumed += n;
                if end {
                    break;
                }
                if consumed > 4 * 1024 * 1024 || started.elapsed() > Duration::from_secs(1) {
                    return Err("source_read_budget");
                }
            }
            records.push((json!({"kind":"unknown","text":"此条原生记录超过单行读取限制，未解析。","detailsOmitted":true}),json!({"offset":start})));
        } else if bytes.last() != Some(&b'\n') {
            records.push((
                json!({"kind":"unknown","text":"来源末尾尚未写完，未作为完整消息读取。"}),
                json!({"offset":start}),
            ));
        } else {
            file.bytes = 0;
            file.document.records.clear();
            file.parse(&bytes, source, key);
            if let Some(mut record) = file.document.records.pop() {
                if details {
                    let raw: Value =
                        serde_json::from_slice(&bytes).map_err(|_| "source_read_failed")?;
                    record["raw"] = sanitize(&raw, key);
                }
                records.push((record, json!({"offset":start,"mirror":mirror})));
            }
        }
        next = Some(json!({"offset":file.offset,"mirror":file.last_message}));
        if consumed > 512 * 1024 || started.elapsed() > Duration::from_secs(1) {
            break;
        }
    }
    if file
        .reader
        .fill_buf()
        .map_err(|_| "source_read_failed")?
        .is_empty()
    {
        next = None;
    }
    if signature(&file.file.metadata().map_err(|_| "source_read_failed")?) != file.signature
        || signature(
            &root
                .read_file(path)
                .map_err(|_| "source_revision_changed")?
                .metadata()
                .map_err(|_| "source_read_failed")?,
        ) != file.signature
    {
        return Err("source_revision_changed");
    }
    Ok(super::window::Window {
        records,
        next,
        reasons: vec![],
    })
}

#[cfg(test)]
#[path = "assignment_tests.rs"]
mod assignment_tests;
