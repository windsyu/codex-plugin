use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::clock::now_ms;
use crate::config::{Config, SourceConfig};
use crate::domain::classify::{classify_item, summary_text};
use crate::domain::identity::{stable_source_id, thread_key};
use crate::domain::model::{ImportReport, NormalizedEvent, OwnedIngestBatch};
use crate::domain::normalize::parse_time_ms;
use crate::domain::redact;
use crate::ingest::io::thread_id_from_filename;
use crate::ingest::keys::load_or_create_key;
use crate::store::Database;
use crate::writer::WriterHandle;

pub struct Importer<'a> {
    config: &'a Config,
    database: &'a Database,
    writer: Option<&'a WriterHandle>,
    fingerprint_key: [u8; 32],
}

struct LineContext<'a> {
    source_id: &'a str,
    epoch_id: &'a str,
    source_seq: i64,
    thread_key: &'a str,
    thread_id: &'a str,
    current_turn: Option<&'a str>,
}

struct BoundedRecord {
    line: Vec<u8>,
    bytes_read: u64,
    payload_len: usize,
    terminated: bool,
    oversize_fingerprint: Option<String>,
}

impl<'a> Importer<'a> {
    pub fn new(config: &'a Config, database: &'a Database) -> Result<Self> {
        let fingerprint_key = load_or_create_key(&config.storage.fingerprint_key_file)?;
        Ok(Self {
            config,
            database,
            writer: None,
            fingerprint_key,
        })
    }

    pub fn new_with_writer(
        config: &'a Config,
        database: &'a Database,
        writer: &'a WriterHandle,
    ) -> Result<Self> {
        let mut importer = Self::new(config, database)?;
        importer.writer = Some(writer);
        Ok(importer)
    }

    pub fn import_all(&self) -> Result<ImportReport> {
        let mut report = ImportReport {
            files_scanned: 0,
            events_inserted: 0,
            events_deduplicated: 0,
            decode_errors: 0,
            sources_degraded: 0,
        };
        for source in &self.config.sources {
            match self.import_source(source, &mut report) {
                Ok(()) => {}
                Err(error) => {
                    report.sources_degraded += 1;
                    tracing::error!(source = %source.name, error = %error, "source import degraded");
                }
            }
        }
        Ok(report)
    }

    fn import_source(&self, source: &SourceConfig, report: &mut ImportReport) -> Result<()> {
        let stable_identity = source.codex_home.to_string_lossy().to_string();
        let source_id = stable_source_id(&stable_identity);
        let source_config =
            json!({"name":source.name,"codexHome":stable_identity,"liveMode":"off"});
        let status = if source.codex_home.is_dir() {
            "ready"
        } else {
            "degraded"
        };
        if let Some(writer) = self.writer {
            writer.upsert_source(&source_id, &stable_identity, &source_config, status)?;
        } else {
            self.database
                .upsert_source(&source_id, &stable_identity, &source_config, status)?;
        }
        if !source.codex_home.is_dir() {
            anyhow::bail!("Codex home {} does not exist", source.codex_home.display());
        }

        for path in discover_rollouts(&source.codex_home)? {
            report.files_scanned += 1;
            if let Err(error) = self.import_file(&source_id, &path, report) {
                report.sources_degraded += 1;
                tracing::warn!(source_id, path = %path.display(), error = %error, "rollout import failed");
            }
        }
        Ok(())
    }

    fn import_file(&self, source_id: &str, path: &Path, report: &mut ImportReport) -> Result<()> {
        let compressed = path.extension().is_some_and(|extension| extension == "zst");
        let representation = if compressed { "zstd" } else { "plain" };
        let metadata =
            fs::metadata(path).with_context(|| format!("stat rollout {}", path.display()))?;
        let identity = file_identity(path, &metadata, compressed)?;
        let checkpoint_key =
            URL_SAFE_NO_PAD.encode(blake3::hash(path.to_string_lossy().as_bytes()).as_bytes());
        let mut checkpoint = self.database.checkpoint(&checkpoint_key)?;
        let reset_epoch = checkpoint
            .file_identity
            .as_deref()
            .is_some_and(|old| old != identity)
            || (!compressed && checkpoint.byte_offset > metadata.len());
        if reset_epoch {
            checkpoint = Default::default();
        }
        let epoch_id = checkpoint
            .epoch_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let (thread_id, session_meta) =
            find_thread_identity(path, compressed).unwrap_or_else(|| {
                (
                    thread_id_from_filename(path)
                        .unwrap_or_else(|| format!("unresolved-{}", &checkpoint_key[..16])),
                    None,
                )
            });
        let thread_key = thread_key(source_id, &thread_id);
        let archived = path
            .components()
            .any(|part| part.as_os_str() == "archived_sessions");

        let start_ordinal = checkpoint.ordinal;
        let mut current_turn = checkpoint.current_turn_id.clone();
        let mut events = Vec::new();
        let mut consumed_bytes = if compressed {
            0
        } else {
            checkpoint.byte_offset
        };
        let mut logical_ordinal = if compressed { 0 } else { start_ordinal };
        let mut clean_eof = true;
        let mut reader = open_rollout_reader(
            path,
            compressed,
            if compressed {
                0
            } else {
                checkpoint.byte_offset
            },
        )?;
        while let Some(record) = read_bounded_record(
            reader.as_mut(),
            &self.fingerprint_key,
            self.config.capture.max_raw_event_bytes,
        )? {
            if !record.terminated && !compressed {
                clean_eof = false;
                break;
            }
            consumed_bytes += record.bytes_read;
            logical_ordinal += 1;
            if logical_ordinal <= start_ordinal {
                if record.oversize_fingerprint.is_none() {
                    update_current_turn_from_line(&record.line, &mut current_turn);
                }
                continue;
            }
            let context = LineContext {
                source_id,
                epoch_id: &epoch_id,
                source_seq: logical_ordinal as i64,
                thread_key: &thread_key,
                thread_id: &thread_id,
                current_turn: current_turn.as_deref(),
            };
            let event = if let Some(fingerprint) = record.oversize_fingerprint {
                self.normalize_record(&[], context, Some(fingerprint), Some(record.payload_len))
            } else {
                self.normalize_line(&record.line, context)
            };
            if event.decode_status == "error" {
                report.decode_errors += 1;
            }
            update_current_turn(&event, &mut current_turn);
            events.push(event);
            if events.len() >= 100 {
                self.commit_batch(
                    source_id,
                    &epoch_id,
                    &checkpoint_key,
                    &identity,
                    if compressed { 0 } else { consumed_bytes },
                    logical_ordinal,
                    current_turn.as_deref(),
                    false,
                    &mut events,
                    report,
                )?;
            }
        }
        self.commit_batch(
            source_id,
            &epoch_id,
            &checkpoint_key,
            &identity,
            if compressed {
                metadata.len()
            } else {
                consumed_bytes
            },
            logical_ordinal,
            current_turn.as_deref(),
            clean_eof,
            &mut events,
            report,
        )?;
        if let Some(writer) = self.writer {
            writer.mark_location(
                source_id,
                &thread_id,
                path.to_path_buf(),
                representation,
                &identity,
                archived,
            )?;
        } else {
            self.database.mark_location(
                source_id,
                &thread_id,
                path,
                representation,
                &identity,
                archived,
            )?;
        }

        if let Some(meta) = session_meta {
            tracing::debug!(source_id, thread_id, session = ?meta.get("session_id"), "rollout identified");
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_batch(
        &self,
        source_id: &str,
        epoch_id: &str,
        checkpoint_key: &str,
        file_identity: &str,
        byte_offset: u64,
        ordinal: u64,
        current_turn_id: Option<&str>,
        clean_eof: bool,
        events: &mut Vec<NormalizedEvent>,
        report: &mut ImportReport,
    ) -> Result<()> {
        if events.is_empty() && !clean_eof {
            return Ok(());
        }
        let batch = OwnedIngestBatch {
            source_id: source_id.to_string(),
            epoch_id: epoch_id.to_string(),
            checkpoint_key: checkpoint_key.to_string(),
            file_identity: file_identity.to_string(),
            byte_offset,
            ordinal,
            current_turn_id: current_turn_id.map(str::to_string),
            clean_eof,
            events: events.clone(),
        };
        let (inserted, deduplicated) = if let Some(writer) = self.writer {
            writer.ingest(batch)?
        } else {
            self.database.ingest_batch(&batch)?
        };
        report.events_inserted += inserted;
        report.events_deduplicated += deduplicated;
        events.clear();
        Ok(())
    }

    fn normalize_line(&self, line: &[u8], context: LineContext<'_>) -> NormalizedEvent {
        self.normalize_record(line, context, None, None)
    }

    fn normalize_record(
        &self,
        line: &[u8],
        context: LineContext<'_>,
        fingerprint_override: Option<String>,
        oversize_len: Option<usize>,
    ) -> NormalizedEvent {
        let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
        let fingerprint = fingerprint_override.unwrap_or_else(|| {
            blake3::keyed_hash(&self.fingerprint_key, trimmed)
                .to_hex()
                .to_string()
        });
        let parsed = if let Some(byte_length) = oversize_len {
            Err(format!(
                "record exceeds max_raw_event_bytes ({})",
                byte_length
            ))
        } else {
            serde_json::from_slice::<Value>(trimmed).map_err(|error| error.to_string())
        };
        let (raw, mut decode_status, decode_error) = match parsed {
            Ok(value) => (value, "decoded".to_string(), None),
            Err(error) => (
                json!({"type":"decode_error","payload":{"byteLength":oversize_len.unwrap_or(trimmed.len()),"fingerprint":&fingerprint[..16]}}),
                "error".to_string(),
                Some(error),
            ),
        };
        let (redacted, mut redaction_audit) = redact::redact(&raw, &self.fingerprint_key);
        let top_type = redacted
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        if decode_status == "decoded" && !is_known_rollout_top_type(&top_type) {
            decode_status = "unknown".into();
        }
        let mut payload = redacted.get("payload").cloned().unwrap_or(Value::Null);
        let nested_type = payload.get("type").and_then(Value::as_str);
        let turn_id =
            extract_turn_id(&payload).or_else(|| context.current_turn.map(str::to_string));
        let item_id = extract_item_id(&payload);
        let event_at_ms = redacted
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_time_ms);
        let method = if top_type == "event_msg" {
            format!("event/{}", nested_type.unwrap_or("unknown"))
        } else if top_type == "response_item" {
            format!("item/{}", nested_type.unwrap_or("unknown"))
        } else {
            format!("rollout/{top_type}")
        };
        let phase = classify_phase(nested_type.unwrap_or(&top_type));
        let item_type = classify_item(&redacted);
        let mut summary_text = summary_text(&redacted);
        let item_status = item_type.as_ref().map(|_| {
            match phase.as_str() {
                "started" => "started",
                "delta" => "streaming",
                "completed" => "completed",
                _ => "completed",
            }
            .to_string()
        });
        let reasoning = item_type.as_deref() == Some("reasoning")
            || nested_type.is_some_and(|kind| kind.to_ascii_lowercase().contains("reasoning"));
        let policy_marker = if reasoning && !self.config.capture.keep_reasoning {
            payload = json!({"policy":"omitted","kind":"reasoning","retainedIdentity":true});
            summary_text = Some("[reasoning omitted by capture policy]".into());
            Some(payload.clone())
        } else if !self.config.capture.keep_raw_json {
            Some(json!({"policy":"omitted","kind":"raw_json","retainedIdentity":true}))
        } else {
            None
        };
        if let Some(marker) = policy_marker.as_ref() {
            redaction_audit["capturePolicy"] = marker.clone();
        }
        let stored = serde_json::to_string(policy_marker.as_ref().unwrap_or(&redacted))
            .expect("JSON serialization cannot fail");
        NormalizedEvent {
            event_id: Uuid::now_v7().to_string(),
            source_id: context.source_id.to_string(),
            store_source_id: context.source_id.to_string(),
            epoch_id: context.epoch_id.to_string(),
            source_seq: context.source_seq,
            dedupe_key: format!(
                "rollout:{}:{}:{}:{fingerprint}",
                context.source_id, context.thread_id, context.source_seq
            ),
            observed_at_ms: now_ms(),
            event_at_ms,
            thread_key: context.thread_key.to_string(),
            codex_thread_id: context.thread_id.to_string(),
            turn_id,
            item_id,
            request_id: None,
            blob_id: None,
            protocol_direction: None,
            worker_id: None,
            worker_connection_epoch: None,
            proxy_seq: None,
            method,
            phase,
            durability: "durable".into(),
            projectable: true,
            source_fingerprint: fingerprint,
            stored_raw_hash: blake3::hash(stored.as_bytes()).to_hex().to_string(),
            raw_json: stored,
            redaction_json: redaction_audit.to_string(),
            decode_status,
            decode_error,
            top_type,
            item_type,
            item_status,
            summary_text,
            payload,
        }
    }
}

fn discover_rollouts(codex_home: &Path) -> Result<Vec<PathBuf>> {
    let mut files = BTreeSet::new();
    for directory in [
        codex_home.join("sessions"),
        codex_home.join("archived_sessions"),
    ] {
        if !directory.is_dir() {
            continue;
        }
        for entry in WalkDir::new(directory).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy();
            if name.starts_with("rollout-")
                && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
            {
                files.insert(entry.into_path());
            }
        }
    }
    Ok(files.into_iter().collect())
}

fn read_bounded_record(
    reader: &mut dyn BufRead,
    fingerprint_key: &[u8; 32],
    max_bytes: usize,
) -> std::io::Result<Option<BoundedRecord>> {
    let mut line = Vec::new();
    let mut bytes_read = 0_u64;
    let mut payload_len = 0_usize;
    let mut terminated = false;
    let mut oversized = false;
    let mut hasher = blake3::Hasher::new_keyed(fingerprint_key);
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let payload = newline.map_or(buffer, |index| &buffer[..index]);
        hasher.update(payload);
        payload_len = payload_len.saturating_add(payload.len());
        if !oversized {
            if payload_len <= max_bytes {
                line.extend_from_slice(payload);
            } else {
                oversized = true;
                line.clear();
                line.shrink_to(0);
            }
        }
        let consumed = payload.len() + usize::from(newline.is_some());
        reader.consume(consumed);
        bytes_read += consumed as u64;
        if newline.is_some() {
            terminated = true;
            if !oversized {
                line.push(b'\n');
            }
            break;
        }
    }
    if bytes_read == 0 {
        return Ok(None);
    }
    Ok(Some(BoundedRecord {
        line,
        bytes_read,
        payload_len,
        terminated,
        oversize_fingerprint: oversized.then(|| hasher.finalize().to_hex().to_string()),
    }))
}

fn open_rollout_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .with_context(|| format!("open rollout {}", path.display()))?;
    if !file.metadata()?.is_file() {
        anyhow::bail!("rollout is not a regular file");
    }
    Ok(file)
}

fn open_rollout_reader(path: &Path, compressed: bool, offset: u64) -> Result<Box<dyn BufRead>> {
    let mut file = open_rollout_file(path)?;
    if compressed {
        let decoder = zstd::stream::read::Decoder::new(file)
            .with_context(|| format!("open zstd rollout {}", path.display()))?;
        Ok(Box::new(BufReader::new(decoder)))
    } else {
        file.seek(SeekFrom::Start(offset))?;
        Ok(Box::new(BufReader::new(file)))
    }
}

fn find_thread_identity(path: &Path, compressed: bool) -> Option<(String, Option<Value>)> {
    let mut reader = open_rollout_reader(path, compressed, 0).ok()?;
    let mut line = Vec::new();
    for _ in 0..64 {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            break;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            let payload = value.get("payload")?;
            if let Some(id) = payload
                .get("id")
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
            {
                return Some((id.to_string(), Some(payload.clone())));
            }
        }
    }
    None
}

fn update_current_turn_from_line(line: &[u8], current: &mut Option<String>) {
    if let Ok(raw) = serde_json::from_slice::<Value>(line)
        && let Some(payload) = raw.get("payload")
        && let Some(turn) = extract_turn_id(payload)
    {
        let nested = payload.get("type").and_then(Value::as_str);
        if matches!(nested, Some("task_started" | "turn_started"))
            || raw.get("type").and_then(Value::as_str) == Some("turn_context")
        {
            *current = Some(turn);
        }
    }
}

fn update_current_turn(event: &NormalizedEvent, current: &mut Option<String>) {
    let nested = event.payload.get("type").and_then(Value::as_str);
    if (event.top_type == "turn_context" || matches!(nested, Some("task_started" | "turn_started")))
        && let Some(turn) = event.turn_id.clone()
    {
        *current = Some(turn);
    }
}

fn extract_turn_id(payload: &Value) -> Option<String> {
    payload
        .get("turn_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            payload
                .pointer("/internal_chat_message_metadata_passthrough/turn_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn extract_item_id(payload: &Value) -> Option<String> {
    ["item_id", "id", "call_id"].iter().find_map(|key| {
        payload
            .get(*key)
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn is_known_rollout_top_type(top_type: &str) -> bool {
    matches!(
        top_type,
        "session_meta"
            | "response_item"
            | "inter_agent_communication"
            | "inter_agent_communication_metadata"
            | "compacted"
            | "turn_context"
            | "world_state"
            | "event_msg"
    )
}

fn classify_phase(kind: &str) -> String {
    if kind.ends_with("_begin")
        || kind.ends_with("_started")
        || kind == "task_started"
        || kind == "turn_started"
    {
        "started"
    } else if kind.ends_with("_delta") || kind.contains("updated") {
        "delta"
    } else if kind == "turn_aborted"
        || kind.ends_with("_end")
        || kind.ends_with("_complete")
        || kind.ends_with("_completed")
    {
        "completed"
    } else if kind.contains("request") || kind.contains("approval") {
        "request"
    } else {
        "snapshot"
    }
    .to_string()
}

fn file_identity(path: &Path, metadata: &fs::Metadata, compressed: bool) -> Result<String> {
    let mut identity = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            format!("{}:{}", metadata.dev(), metadata.ino())
        }
        #[cfg(not(unix))]
        {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|v| v.as_nanos())
                .unwrap_or(0);
            format!("{}:{}:{}", path.display(), metadata.len(), modified)
        }
    };
    if compressed {
        let mut file = open_rollout_file(path)?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        identity.push(':');
        identity.push_str(hasher.finalize().to_hex().as_str());
    }
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use crate::credentials::load_or_create_token;
    use tempfile::TempDir;

    fn test_config(temp: &TempDir) -> Config {
        let mut config = Config {
            config_dir: temp.path().to_path_buf(),
            ..Config::default()
        };
        config.storage.database = temp.path().join("observer.sqlite");
        config.storage.fingerprint_key_file = temp.path().join("fingerprint.key");
        config.server.bearer_token_file = temp.path().join("token");
        config.sources[0].codex_home = temp.path().join("codex");
        config
    }

    #[test]
    fn partial_line_is_not_committed_and_reimport_is_idempotent() -> Result<()> {
        let temp = TempDir::new()?;
        let config = test_config(&temp);
        let sessions = config.sources[0].codex_home.join("sessions/2026/08/14");
        fs::create_dir_all(&sessions)?;
        let rollout = sessions.join("rollout-test-thread-1.jsonl");
        fs::write(
            &rollout,
            concat!(
                "{\"timestamp\":\"2026-08-14T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"thread-1\",\"session_id\":\"session-1\",\"cwd\":\"/tmp/project\"}}\n",
                "{\"timestamp\":\"2026-08-14T00:00:01Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"turn_id\":\"turn-1\"}}\n",
                "{\"timestamp\":\"2026-08-14T00:00:02Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"partial"
            ),
        )?;
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        database.migrate()?;
        let importer = Importer::new(&config, &database)?;
        let first = importer.import_all()?;
        assert_eq!(first.events_inserted, 2);
        let second = importer.import_all()?;
        assert_eq!(second.events_inserted, 0);

        let mut file = OpenOptions::new().append(true).open(&rollout)?;
        writeln!(file, " complete\"}}")?;
        let third = importer.import_all()?;
        assert_eq!(third.events_inserted, 1);
        assert_eq!(database.max_event_seq()?, 3);
        Ok(())
    }

    #[test]
    fn periodic_rescan_recovers_when_watcher_hint_is_lost() -> Result<()> {
        let temp = TempDir::new()?;
        let config = test_config(&temp);
        let sessions = config.sources[0].codex_home.join("sessions");
        fs::create_dir_all(&sessions)?;
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        let importer = Importer::new(&config, &database)?;
        assert_eq!(importer.import_all()?.events_inserted, 0);
        fs::write(
            sessions.join("rollout-rescan.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"rescan-thread\"}}\n",
        )?;
        let recovered = importer.import_all()?;
        assert_eq!(recovered.events_inserted, 1);
        assert_eq!(database.max_event_seq()?, 1);
        Ok(())
    }

    #[test]
    fn thread_completeness_aggregates_every_turn_and_clean_eof() -> Result<()> {
        let temp = TempDir::new()?;
        let config = test_config(&temp);
        let sessions = config.sources[0].codex_home.join("sessions");
        fs::create_dir_all(&sessions)?;
        let rollout = sessions.join("rollout-completeness-thread.jsonl");
        let mut file = File::create(&rollout)?;
        for value in [
            json!({"type":"session_meta","payload":{"id":"completeness-thread"}}),
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}),
            json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1"}}),
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}),
        ] {
            writeln!(file, "{value}")?;
        }
        file.sync_all()?;
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        let importer = Importer::new(&config, &database)?;
        importer.import_all()?;
        let connection = database.connect()?;
        let thread_state: String = connection.query_row(
            "SELECT capture_completeness FROM threads WHERE codex_thread_id='completeness-thread'",
            [],
            |row| row.get(0),
        )?;
        let turn_states = {
            let mut statement = connection.prepare(
                "SELECT turn_id,capture_completeness,json_extract(coverage_json,'$.durableEofReached')
                 FROM turns ORDER BY turn_id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        assert_eq!(thread_state, "durable_partial");
        assert_eq!(
            turn_states,
            vec![
                ("turn-1".into(), "durable_complete".into(), true),
                ("turn-2".into(), "durable_partial".into(), true)
            ]
        );
        drop(connection);

        writeln!(
            file,
            "{}",
            json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2"}})
        )?;
        file.sync_all()?;
        importer.import_all()?;
        let complete: String = database.connect()?.query_row(
            "SELECT capture_completeness FROM threads WHERE codex_thread_id='completeness-thread'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(complete, "durable_complete");
        Ok(())
    }

    #[test]
    fn redaction_v2_secrets_never_reach_database_or_export() -> Result<()> {
        let temp = TempDir::new()?;
        let config = test_config(&temp);
        let sessions = config.sources[0].codex_home.join("sessions");
        fs::create_dir_all(&sessions)?;
        let rollout = sessions.join("rollout-redaction-v2.jsonl");
        let media = format!("data:image/png;base64,{}", "A".repeat(256));
        let mut file = File::create(&rollout)?;
        writeln!(
            file,
            "{}",
            json!({"type":"session_meta","payload":{"id":"redaction-v2"}})
        )?;
        writeln!(
            file,
            "{}",
            json!({"type":"event_msg","payload":{"type":"mcp_tool_call_begin","turn_id":"turn-redact",
              "url":"https://example.test/?token=url-secret&view=ok",
              "auth":{"bearerToken":"mcp-secret"},"image":media}})
        )?;
        file.sync_all()?;
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        Importer::new(&config, &database)?.import_all()?;
        let raw: String = database.connect()?.query_row(
            "SELECT group_concat(raw_json,'') FROM raw_events",
            [],
            |row| row.get(0),
        )?;
        assert!(!raw.contains("url-secret"));
        assert!(!raw.contains("mcp-secret"));
        assert!(!raw.contains(&"A".repeat(128)));
        assert!(raw.contains("media_payload"));
        let thread_key: String = database.connect()?.query_row(
            "SELECT thread_key FROM threads WHERE codex_thread_id='redaction-v2'",
            [],
            |row| row.get(0),
        )?;
        let output = temp.path().join("redaction-export.json");
        let report = database.export_thread(&thread_key, &output)?;
        assert_eq!(report.legacy_redaction_events, 0);
        let exported = fs::read_to_string(output)?;
        assert!(!exported.contains("url-secret"));
        assert!(!exported.contains("mcp-secret"));
        assert!(!exported.contains(&"A".repeat(128)));
        Ok(())
    }

    #[test]
    fn capture_policy_omits_reasoning_and_raw_json_without_losing_projection() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = test_config(&temp);
        config.capture.keep_reasoning = false;
        config.capture.keep_raw_json = false;
        let sessions = config.sources[0].codex_home.join("sessions");
        fs::create_dir_all(&sessions)?;
        let rollout = sessions.join("rollout-policy.jsonl");
        let mut file = File::create(&rollout)?;
        for value in [
            json!({"type":"session_meta","payload":{"id":"policy-thread"}}),
            json!({"type":"response_item","payload":{"type":"reasoning","id":"reasoning-1","turn_id":"turn-1","summary":[{"text":"private-reasoning-text"}]}}),
            json!({"type":"event_msg","payload":{"type":"agent_message","turn_id":"turn-1","message":"visible-answer"}}),
        ] {
            writeln!(file, "{value}")?;
        }
        file.sync_all()?;
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        Importer::new(&config, &database)?.import_all()?;
        let connection = database.connect()?;
        let stored: String = connection.query_row(
            "SELECT group_concat(raw_json,'') FROM raw_events",
            [],
            |row| row.get(0),
        )?;
        assert!(!stored.contains("private-reasoning-text"));
        assert!(!stored.contains("visible-answer"));
        assert!(stored.contains("omitted"));
        let projection: String = connection.query_row(
            "SELECT summary_text FROM items WHERE summary_text='visible-answer'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(projection, "visible-answer");
        let leaked_search: i64 = connection.query_row(
            "SELECT COUNT(*) FROM search_index WHERE text LIKE '%private-reasoning-text%'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(leaked_search, 0);
        Ok(())
    }

    #[test]
    fn bad_json_does_not_block_later_records() -> Result<()> {
        let temp = TempDir::new()?;
        let config = test_config(&temp);
        let sessions = config.sources[0].codex_home.join("sessions");
        fs::create_dir_all(&sessions)?;
        fs::write(
            sessions.join("rollout-thread-2.jsonl"),
            concat!(
                "{\"timestamp\":\"2026-08-14T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"thread-2\"}}\n",
                "not json\n",
                "{\"timestamp\":\"2026-08-14T00:00:01Z\",\"type\":\"future_variant\",\"payload\":{\"hello\":\"world\"}}\n"
            ),
        )?;
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        let report = Importer::new(&config, &database)?.import_all()?;
        assert_eq!(report.events_inserted, 3);
        assert_eq!(report.decode_errors, 1);
        Ok(())
    }

    #[test]
    fn oversized_line_is_bounded_audited_and_does_not_block_next_record() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = test_config(&temp);
        config.capture.max_raw_event_bytes = 1024;
        let sessions = config.sources[0].codex_home.join("sessions");
        fs::create_dir_all(&sessions)?;
        let rollout = sessions.join("rollout-thread-oversize.jsonl");
        let mut file = File::create(&rollout)?;
        writeln!(
            file,
            "{}",
            json!({"type":"session_meta","payload":{"id":"thread-oversize"}})
        )?;
        writeln!(file, "{}", "x".repeat(16 * 1024))?;
        writeln!(
            file,
            "{}",
            json!({"type":"future_variant","payload":{"after":true}})
        )?;
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        let report = Importer::new(&config, &database)?.import_all()?;
        assert_eq!(report.events_inserted, 3);
        assert_eq!(report.decode_errors, 1);
        let connection = database.connect()?;
        let (status, byte_length): (String, i64) = connection.query_row(
            "SELECT decode_status,json_extract(raw_json,'$.payload.byteLength')
             FROM raw_events WHERE source_seq=2",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(status, "error");
        assert!(byte_length > 1024);
        Ok(())
    }

    #[test]
    fn synthetic_golden_fixture_projects_timeline_and_redacts_secret() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = test_config(&temp);
        config.sources[0].codex_home =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex-home");
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        let report = Importer::new(&config, &database)?.import_all()?;
        assert_eq!(report.events_inserted, 21);
        let connection = database.connect()?;
        let threads: i64 =
            connection.query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))?;
        let turns: i64 =
            connection.query_row("SELECT COUNT(*) FROM turns", [], |row| row.get(0))?;
        let items: i64 =
            connection.query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?;
        let complete_threads: i64 = connection.query_row(
            "SELECT COUNT(*) FROM threads WHERE capture_completeness='durable_complete'",
            [],
            |row| row.get(0),
        )?;
        let raw_mcp: String = connection.query_row(
            "SELECT raw_json FROM raw_events WHERE method='event/mcp_tool_call_begin'",
            [],
            |row| row.get(0),
        )?;
        let chinese_search_hits: i64 = connection.query_row(
            "SELECT COUNT(*) FROM search_index WHERE search_index MATCH '\"观察器\"'",
            [],
            |row| row.get(0),
        )?;
        let relation: (bool, bool, String, String, String, String) = connection.query_row(
            "SELECT c.parent_thread_key=p.thread_key,c.forked_from_thread_key=p.thread_key,
               c.agent_nickname,c.agent_role,c.history_mode,c.model_provider
             FROM threads c JOIN threads p ON p.codex_thread_id='00000000-0000-7000-8000-000000000001'
             WHERE c.codex_thread_id='00000000-0000-7000-8000-000000000002'",
            [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
        )?;
        let execution: (String, String, String) = connection.query_row(
            "SELECT model,reasoning_effort,approval_policy FROM threads
             WHERE codex_thread_id='00000000-0000-7000-8000-000000000002'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let sub_agent_items: i64 = connection.query_row(
            "SELECT COUNT(*) FROM items WHERE item_type='sub_agent'",
            [],
            |row| row.get(0),
        )?;
        let unknown_events: i64 = connection.query_row(
            "SELECT COUNT(*) FROM raw_events WHERE decode_status='unknown'
             AND method='rollout/future_observer_fixture_variant'
             AND json_extract(raw_json,'$.payload.note')='unknown variants must remain queryable'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!((threads, turns), (3, 3));
        assert!(items >= 8);
        assert_eq!(complete_threads, 3);
        assert_eq!(
            relation,
            (
                true,
                true,
                "fixture-child".into(),
                "researcher".into(),
                "paginated".into(),
                "openai".into()
            )
        );
        assert_eq!(
            execution,
            (
                "gpt-child-fixture".into(),
                "high".into(),
                "on-request".into()
            )
        );
        assert_eq!(sub_agent_items, 1);
        assert_eq!(unknown_events, 1);
        assert!(chinese_search_hits >= 1);
        assert!(!raw_mcp.contains("fixture-secret"));
        assert!(raw_mcp.contains("$redacted"));
        Ok(())
    }

    #[test]
    fn zstd_rollout_is_supported_and_archive_rename_is_idempotent() -> Result<()> {
        let temp = TempDir::new()?;
        let config = test_config(&temp);
        let sessions = config.sources[0].codex_home.join("sessions");
        let archive = config.sources[0].codex_home.join("archived_sessions");
        fs::create_dir_all(&sessions)?;
        fs::create_dir_all(&archive)?;
        let contents = concat!(
            "{\"timestamp\":\"2026-08-14T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"thread-zstd\"}}\n",
            "{\"timestamp\":\"2026-08-14T00:00:01Z\",\"type\":\"future_variant\",\"payload\":{}}\n"
        );
        let active = sessions.join("rollout-thread-zstd.jsonl.zst");
        {
            let file = File::create(&active)?;
            let mut encoder = zstd::stream::write::Encoder::new(file, 1)?;
            encoder.write_all(contents.as_bytes())?;
            encoder.finish()?;
        }
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        let importer = Importer::new(&config, &database)?;
        assert_eq!(importer.import_all()?.events_inserted, 2);
        let archived = archive.join("rollout-thread-zstd.jsonl.zst");
        fs::rename(active, archived)?;
        assert_eq!(importer.import_all()?.events_inserted, 0);
        assert_eq!(database.max_event_seq()?, 2);
        Ok(())
    }

    #[test]
    fn retention_keeps_dedupe_tombstones_and_projections() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = test_config(&temp);
        config.sources[0].codex_home =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex-home");
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        let importer = Importer::new(&config, &database)?;
        assert_eq!(importer.import_all()?.events_inserted, 21);
        database
            .connect()?
            .execute("UPDATE raw_events SET observed_at_ms=0", [])?;

        let preview = database.run_retention(1, 1, 1, false)?;
        assert_eq!(preview.candidate_raw_events, 21);
        assert_eq!(preview.deleted_raw_events, 0);
        let applied = database.run_retention(1, 1, 1, true)?;
        assert_eq!(applied.deleted_raw_events, 21);
        assert_eq!(applied.dedupe_tombstones_retained, 21);
        assert_eq!(database.max_event_seq()?, 21);
        assert_eq!(database.retention_low_watermark()?, 21);

        let connection = database.connect()?;
        let raw_count: i64 =
            connection.query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))?;
        let thread_count: i64 =
            connection.query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))?;
        assert_eq!(raw_count, 0);
        assert_eq!(thread_count, 3);
        connection.execute("DELETE FROM source_checkpoints", [])?;
        let replay = importer.import_all()?;
        assert_eq!(replay.events_inserted, 0);
        assert_eq!(replay.events_deduplicated, 21);
        assert!(database.rebuild_projections().is_err());
        Ok(())
    }

    #[test]
    fn large_raw_event_uses_rebuildable_blob_and_retention_keeps_references_consistent()
    -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = test_config(&temp);
        config.storage.blob_dir = temp.path().join("blob-store");
        config.capture.inline_blob_bytes = 1024;
        let sessions = config.sources[0].codex_home.join("sessions");
        fs::create_dir_all(&sessions)?;
        let rollout = sessions.join("rollout-thread-blob.jsonl");
        let large_text = format!("large-marker-{}", "x".repeat(4096));
        let records = [
            json!({"timestamp":"2026-08-14T00:00:00Z","type":"session_meta","payload":{"id":"thread-blob"}}),
            json!({"timestamp":"2026-08-14T00:00:01Z","type":"turn_context","payload":{"turn_id":"turn-blob"}}),
            json!({"timestamp":"2026-08-14T00:00:02Z","type":"response_item","payload":{
                "type":"message","id":"message-blob","role":"assistant","api_key":"fixture-secret",
                "content":[{"type":"output_text","text":large_text}],
                "internal_chat_message_metadata_passthrough":{"turn_id":"turn-blob"}}}),
        ];
        let mut contents = records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        contents.push('\n');
        fs::write(&rollout, contents)?;
        let database = Database::open_with_blobs(
            &config.storage.database,
            &config.storage.blob_dir,
            config.capture.inline_blob_bytes,
        )?;
        database.migrate()?;
        let importer = Importer::new(&config, &database)?;
        assert_eq!(importer.import_all()?.events_inserted, 3);

        let (blob_id, raw_stub, projection): (String, String, String) =
            database.connect()?.query_row(
                "SELECT r.blob_id,r.raw_json,i.projection_json FROM raw_events r
                 JOIN items i ON i.thread_key=r.thread_key AND i.item_id=r.item_id
                 WHERE r.item_id='message-blob'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        assert!(raw_stub.contains("blobRef"));
        assert!(projection.contains("blobRefs"));
        assert!(!projection.contains("large-marker"));
        let record = database.blob_record(&blob_id)?.context("blob record")?;
        let blob_path = record.path.clone();
        let mut materialized = String::new();
        database
            .open_blob(&record)?
            .read_to_string(&mut materialized)?;
        assert!(materialized.contains("large-marker"));
        assert!(!materialized.contains("fixture-secret"));
        assert_eq!(database.rebuild_projections()?, 3);
        assert!(database.blob_record(&blob_id)?.is_some());

        let inline = json!({"timestamp":"2026-08-14T00:00:03Z","type":"response_item","payload":{
            "type":"message","id":"message-blob","role":"assistant",
            "content":[{"type":"output_text","text":"small replacement"}],
            "internal_chat_message_metadata_passthrough":{"turn_id":"turn-blob"}}});
        let mut file = OpenOptions::new().append(true).open(&rollout)?;
        writeln!(file, "{inline}")?;
        assert_eq!(importer.import_all()?.events_inserted, 1);
        let connection = database.connect()?;
        connection.execute(
            "UPDATE raw_events SET observed_at_ms=0 WHERE blob_id=?1",
            [&blob_id],
        )?;
        connection.execute(
            "UPDATE blobs SET created_at_ms=0 WHERE blob_id=?1",
            [&blob_id],
        )?;
        drop(connection);
        let preview = database.run_retention(1, 1, 1, false)?;
        assert_eq!(preview.candidate_blobs, 1);
        let applied = database.run_retention(1, 1, 1, true)?;
        assert_eq!(applied.deleted_blobs, 1);
        assert!(database.blob_record(&blob_id)?.is_none());
        assert!(!blob_path.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn private_credentials_reject_symlinks_and_repair_broad_permissions() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = TempDir::new()?;
        let target = temp.path().join("target-token");
        fs::write(&target, "secret")?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
        let link = temp.path().join("token-link");
        symlink(&target, &link)?;
        assert!(load_or_create_token(&link).is_err());

        let broad = temp.path().join("broad-token");
        fs::write(&broad, "secret")?;
        fs::set_permissions(&broad, fs::Permissions::from_mode(0o644))?;
        assert_eq!(load_or_create_token(&broad)?, "secret");
        assert_eq!(fs::metadata(&broad)?.permissions().mode() & 0o777, 0o600);
        Ok(())
    }

    #[test]
    fn importer_streams_more_than_one_transaction_batch() -> Result<()> {
        let temp = TempDir::new()?;
        let config = test_config(&temp);
        let sessions = config.sources[0].codex_home.join("sessions");
        fs::create_dir_all(&sessions)?;
        let rollout = sessions.join("rollout-thread-batches.jsonl");
        let mut file = File::create(&rollout)?;
        writeln!(
            file,
            "{}",
            json!({"type":"session_meta","payload":{"id":"thread-batches"}})
        )?;
        for ordinal in 1..=1200 {
            writeln!(
                file,
                "{}",
                json!({"type":"future_variant","payload":{"ordinal":ordinal}})
            )?;
        }
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        let report = Importer::new(&config, &database)?.import_all()?;
        assert_eq!(report.events_inserted, 1201);
        let connection = database.connect()?;
        let (events, ordinal): (i64, i64) = connection.query_row(
            "SELECT (SELECT COUNT(*) FROM raw_events),ordinal FROM source_checkpoints LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!((events, ordinal), (1201, 1201));
        Ok(())
    }

    #[test]
    fn identical_thread_ids_in_two_codex_homes_remain_isolated() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = test_config(&temp);
        let first_home = temp.path().join("codex-a");
        let second_home = temp.path().join("codex-b");
        let rollout = concat!(
            "{\"timestamp\":\"2026-08-14T00:00:00Z\",\"type\":\"session_meta\",",
            "\"payload\":{\"id\":\"same-thread-id\",\"cwd\":\"/fixture\"}}\n"
        );
        for home in [&first_home, &second_home] {
            let sessions = home.join("sessions/2026/08/14");
            fs::create_dir_all(&sessions)?;
            fs::write(sessions.join("rollout-same-thread-id.jsonl"), rollout)?;
        }
        config.sources[0].name = "source-a".into();
        config.sources[0].codex_home = first_home;
        let mut second = config.sources[0].clone();
        second.name = "source-b".into();
        second.codex_home = second_home;
        config.sources.push(second);
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        Importer::new(&config, &database)?.import_all()?;
        let connection = database.connect()?;
        let (threads, thread_keys, sources): (i64, i64, i64) = connection.query_row(
            "SELECT COUNT(*),COUNT(DISTINCT thread_key),COUNT(DISTINCT store_source_id)
             FROM threads WHERE codex_thread_id='same-thread-id'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!((threads, thread_keys, sources), (2, 2, 2));
        Ok(())
    }

    #[test]
    fn changed_zstd_content_resets_epoch_even_when_inode_is_reused() -> Result<()> {
        let temp = TempDir::new()?;
        let config = test_config(&temp);
        let sessions = config.sources[0].codex_home.join("sessions");
        fs::create_dir_all(&sessions)?;
        let rollout = sessions.join("rollout-thread-zstd-fingerprint.jsonl.zst");
        let write_zstd = |version: i64| -> Result<()> {
            let file = File::create(&rollout)?;
            let mut encoder = zstd::stream::write::Encoder::new(file, 1)?;
            writeln!(
                encoder,
                "{}",
                json!({"type":"session_meta","payload":{"id":"thread-zstd-fingerprint"}})
            )?;
            writeln!(
                encoder,
                "{}",
                json!({"type":"future_variant","payload":{"version":version}})
            )?;
            encoder.finish()?;
            Ok(())
        };
        write_zstd(1)?;
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        let importer = Importer::new(&config, &database)?;
        assert_eq!(importer.import_all()?.events_inserted, 2);
        write_zstd(2)?;
        assert_eq!(importer.import_all()?.events_inserted, 1);
        assert_eq!(database.max_event_seq()?, 3);
        Ok(())
    }

    #[test]
    fn plain_and_zstd_siblings_share_logical_dedupe_identity() -> Result<()> {
        let temp = TempDir::new()?;
        let config = test_config(&temp);
        let sessions = config.sources[0].codex_home.join("sessions");
        fs::create_dir_all(&sessions)?;
        let contents = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"thread-sibling\"}}\n",
            "{\"type\":\"future_variant\",\"payload\":{}}\n"
        );
        fs::write(sessions.join("rollout-thread-sibling.jsonl"), contents)?;
        let compressed = File::create(sessions.join("rollout-thread-sibling.jsonl.zst"))?;
        let mut encoder = zstd::stream::write::Encoder::new(compressed, 1)?;
        encoder.write_all(contents.as_bytes())?;
        encoder.finish()?;
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        let report = Importer::new(&config, &database)?.import_all()?;
        assert_eq!(report.files_scanned, 2);
        assert_eq!(report.events_inserted, 2);
        assert_eq!(report.events_deduplicated, 2);
        assert_eq!(database.max_event_seq()?, 2);
        Ok(())
    }

    #[test]
    fn filename_fallback_preserves_complete_uuid_and_rollout_slug() {
        assert_eq!(
            thread_id_from_filename(Path::new(
                "rollout-2026-08-14T12-00-00-00000000-0000-7000-8000-000000000001.jsonl"
            )),
            Some("00000000-0000-7000-8000-000000000001".into())
        );
        assert_eq!(
            thread_id_from_filename(Path::new("rollout-thread-with-slug.jsonl.zst")),
            Some("thread-with-slug".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn rollout_reader_rejects_symlink_target() -> Result<()> {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new()?;
        let target = temp.path().join("target.jsonl");
        fs::write(&target, "{}\n")?;
        let link = temp.path().join("rollout-link.jsonl");
        symlink(target, &link)?;
        assert!(open_rollout_reader(&link, false, 0).is_err());
        Ok(())
    }
}
