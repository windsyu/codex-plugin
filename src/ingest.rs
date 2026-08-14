use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::DateTime;
use rand::RngCore;
use serde_json::{Value, json};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::config::{Config, SourceConfig};
use crate::db::{Database, IngestBatch, classify_item, now_ms, summary_text};
use crate::model::{ImportReport, NormalizedEvent};
use crate::redact;

pub struct Importer<'a> {
    config: &'a Config,
    database: &'a Database,
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

impl<'a> Importer<'a> {
    pub fn new(config: &'a Config, database: &'a Database) -> Result<Self> {
        let fingerprint_key = load_or_create_key(&config.storage.fingerprint_key_file)?;
        Ok(Self {
            config,
            database,
            fingerprint_key,
        })
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
        self.database.upsert_source(
            &source_id,
            &stable_identity,
            &json!({"name":source.name,"codexHome":stable_identity,"liveMode":"off"}),
            if source.codex_home.is_dir() {
                "ready"
            } else {
                "degraded"
            },
        )?;
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
        let identity = file_identity(path, &metadata);
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

        let bytes = if compressed {
            let input = File::open(path)?;
            let mut decoder = zstd::stream::read::Decoder::new(input)
                .with_context(|| format!("open zstd rollout {}", path.display()))?;
            let mut bytes = Vec::new();
            decoder
                .read_to_end(&mut bytes)
                .with_context(|| format!("decode zstd rollout {}", path.display()))?;
            bytes
        } else {
            fs::read(path).with_context(|| format!("read rollout {}", path.display()))?
        };

        let (thread_id, session_meta) = find_thread_identity(&bytes).unwrap_or_else(|| {
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
        let mut consumed_bytes = 0_u64;
        let mut logical_ordinal = 0_u64;
        let mut reader = BufReader::new(bytes.as_slice());
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            if !line.ends_with(b"\n") && !compressed {
                break;
            }
            consumed_bytes += read as u64;
            logical_ordinal += 1;
            if logical_ordinal <= start_ordinal {
                update_current_turn_from_line(&line, &mut current_turn);
                continue;
            }
            let event = self.normalize_line(
                &line,
                LineContext {
                    source_id,
                    epoch_id: &epoch_id,
                    source_seq: logical_ordinal as i64,
                    thread_key: &thread_key,
                    thread_id: &thread_id,
                    current_turn: current_turn.as_deref(),
                },
            );
            if event.decode_status == "error" {
                report.decode_errors += 1;
            }
            update_current_turn(&event, &mut current_turn);
            events.push(event);
        }

        // Compressed rollouts are immutable, so a final non-newline record is complete.
        if compressed && consumed_bytes < bytes.len() as u64 {
            let rest = &bytes[consumed_bytes as usize..];
            if !rest.is_empty() {
                logical_ordinal += 1;
                if logical_ordinal > start_ordinal {
                    let event = self.normalize_line(
                        rest,
                        LineContext {
                            source_id,
                            epoch_id: &epoch_id,
                            source_seq: logical_ordinal as i64,
                            thread_key: &thread_key,
                            thread_id: &thread_id,
                            current_turn: current_turn.as_deref(),
                        },
                    );
                    if event.decode_status == "error" {
                        report.decode_errors += 1;
                    }
                    update_current_turn(&event, &mut current_turn);
                    events.push(event);
                }
                consumed_bytes = bytes.len() as u64;
            }
        }

        let (inserted, deduplicated) = self.database.ingest_batch(IngestBatch {
            source_id,
            epoch_id: &epoch_id,
            checkpoint_key: &checkpoint_key,
            file_identity: &identity,
            byte_offset: if compressed {
                metadata.len()
            } else {
                consumed_bytes
            },
            ordinal: logical_ordinal,
            current_turn_id: current_turn.as_deref(),
            events: &events,
        })?;
        report.events_inserted += inserted;
        report.events_deduplicated += deduplicated;
        self.database.mark_location(
            source_id,
            &thread_id,
            path,
            representation,
            &identity,
            archived,
        )?;

        if let Some(meta) = session_meta {
            tracing::debug!(source_id, thread_id, session = ?meta.get("session_id"), "rollout identified");
        }
        Ok(())
    }

    fn normalize_line(&self, line: &[u8], context: LineContext<'_>) -> NormalizedEvent {
        let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
        let fingerprint = blake3::keyed_hash(&self.fingerprint_key, trimmed)
            .to_hex()
            .to_string();
        let parsed = if trimmed.len() > self.config.capture.max_raw_event_bytes {
            Err(format!(
                "record exceeds max_raw_event_bytes ({})",
                trimmed.len()
            ))
        } else {
            serde_json::from_slice::<Value>(trimmed).map_err(|error| error.to_string())
        };
        let (raw, decode_status, decode_error) = match parsed {
            Ok(value) => (value, "decoded".to_string(), None),
            Err(error) => (
                json!({"type":"decode_error","payload":{"byteLength":trimmed.len(),"fingerprint":&fingerprint[..16]}}),
                "error".to_string(),
                Some(error),
            ),
        };
        let (redacted, redaction_audit) = redact::redact(&raw);
        let stored = serde_json::to_string(&redacted).expect("JSON serialization cannot fail");
        let top_type = redacted
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let payload = redacted.get("payload").cloned().unwrap_or(Value::Null);
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
        let summary_text = summary_text(&redacted);
        let item_status = item_type.as_ref().map(|_| {
            match phase.as_str() {
                "started" => "started",
                "delta" => "streaming",
                "completed" => "completed",
                _ => "completed",
            }
            .to_string()
        });
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

fn find_thread_identity(bytes: &[u8]) -> Option<(String, Option<Value>)> {
    let reader = BufReader::new(bytes);
    for line in reader.lines().take(64) {
        let Ok(line) = line else { continue };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            let payload = value.get("payload")?;
            if let Some(id) = payload.get("id").and_then(Value::as_str) {
                return Some((id.to_string(), Some(payload.clone())));
            }
        }
    }
    None
}

fn thread_id_from_filename(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".zst")
        .unwrap_or(name)
        .strip_suffix(".jsonl")?;
    stem.rsplit('-').next().map(str::to_string)
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

fn classify_phase(kind: &str) -> String {
    if kind.ends_with("_begin")
        || kind.ends_with("_started")
        || kind == "task_started"
        || kind == "turn_started"
    {
        "started"
    } else if kind.ends_with("_delta") || kind.contains("updated") {
        "delta"
    } else if kind.ends_with("_end") || kind.ends_with("_complete") || kind.ends_with("_completed")
    {
        "completed"
    } else if kind.contains("request") || kind.contains("approval") {
        "request"
    } else {
        "snapshot"
    }
    .to_string()
}

pub(crate) fn stable_source_id(identity: &str) -> String {
    URL_SAFE_NO_PAD
        .encode(blake3::hash(format!("store\0{identity}\0{identity}").as_bytes()).as_bytes())
}

pub(crate) fn thread_key(source_id: &str, thread_id: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{source_id}\0{thread_id}"))
}

fn file_identity(_path: &Path, metadata: &fs::Metadata) -> String {
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
        format!("{}:{}:{}", _path.display(), metadata.len(), modified)
    }
}

pub(crate) fn load_or_create_key(path: &Path) -> Result<[u8; 32]> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        validate_private_parent(parent, "fingerprint key")?;
    }
    if fs::symlink_metadata(path).is_ok() {
        validate_private_file(path, "fingerprint key")?;
        let bytes =
            fs::read(path).with_context(|| format!("read fingerprint key {}", path.display()))?;
        return bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("fingerprint key must contain exactly 32 bytes"));
    }
    let mut key = [0_u8; 32];
    rand::rng().fill_bytes(&mut key);
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("create fingerprint key {}", path.display()))?;
    file.write_all(&key)?;
    file.sync_all()?;
    Ok(key)
}

pub fn load_or_create_token(path: &Path) -> Result<String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        validate_private_parent(parent, "bearer token")?;
    }
    if fs::symlink_metadata(path).is_ok() {
        validate_private_file(path, "bearer token")?;
        return Ok(fs::read_to_string(path)?.trim().to_string());
    }
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    writeln!(file, "{token}")?;
    file.sync_all()?;
    Ok(token)
}

fn validate_private_file(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("{label} must be a direct regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            anyhow::bail!("{label} must be owned by the current user with mode 0600");
        }
        validate_private_parent(path.parent().context("private file has no parent")?, label)?;
    }
    Ok(())
}

fn validate_private_parent(path: &Path, label: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(path)?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            anyhow::bail!("{label} directory must not be writable by group or other users");
        }
    }
    #[cfg(not(unix))]
    let _ = (path, label);
    Ok(())
}

fn parse_time_ms(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn synthetic_golden_fixture_projects_timeline_and_redacts_secret() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = test_config(&temp);
        config.sources[0].codex_home =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex-home");
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        let report = Importer::new(&config, &database)?.import_all()?;
        assert_eq!(report.events_inserted, 12);
        let connection = database.connect()?;
        let threads: i64 =
            connection.query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))?;
        let turns: i64 =
            connection.query_row("SELECT COUNT(*) FROM turns", [], |row| row.get(0))?;
        let items: i64 =
            connection.query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?;
        let completeness: String =
            connection.query_row("SELECT capture_completeness FROM threads", [], |row| {
                row.get(0)
            })?;
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
        assert_eq!((threads, turns), (1, 1));
        assert!(items >= 6);
        assert_eq!(completeness, "durable_complete");
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
        assert_eq!(importer.import_all()?.events_inserted, 12);
        database
            .connect()?
            .execute("UPDATE raw_events SET observed_at_ms=0", [])?;

        let preview = database.run_retention(1, 1, false)?;
        assert_eq!(preview.candidate_raw_events, 12);
        assert_eq!(preview.deleted_raw_events, 0);
        let applied = database.run_retention(1, 1, true)?;
        assert_eq!(applied.deleted_raw_events, 12);
        assert_eq!(applied.dedupe_tombstones_retained, 12);
        assert_eq!(database.max_event_seq()?, 12);
        assert_eq!(database.retention_low_watermark()?, 12);

        let connection = database.connect()?;
        let raw_count: i64 =
            connection.query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))?;
        let thread_count: i64 =
            connection.query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))?;
        assert_eq!(raw_count, 0);
        assert_eq!(thread_count, 1);
        connection.execute("DELETE FROM source_checkpoints", [])?;
        let replay = importer.import_all()?;
        assert_eq!(replay.events_inserted, 0);
        assert_eq!(replay.events_deduplicated, 12);
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
        let preview = database.run_retention(1, 1, false)?;
        assert_eq!(preview.candidate_blobs, 1);
        let applied = database.run_retention(1, 1, true)?;
        assert_eq!(applied.deleted_blobs, 1);
        assert!(database.blob_record(&blob_id)?.is_none());
        assert!(!blob_path.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn private_credentials_reject_symlinks_and_broad_permissions() -> Result<()> {
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
        assert!(load_or_create_token(&broad).is_err());
        Ok(())
    }
}
