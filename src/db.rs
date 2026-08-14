use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

use crate::config::Config;
use crate::model::{Checkpoint, DoctorReport, DoctorSource, NormalizedEvent, RetentionReport};

const MIGRATION_1: &str = include_str!("../migrations/0001_initial.sql");
const MIGRATION_2: &str = include_str!("../migrations/0002_fts_trigram.sql");
const MIGRATION_3: &str = include_str!("../migrations/0003_retention.sql");

#[derive(Debug)]
pub struct Database {
    path: PathBuf,
}

pub struct IngestBatch<'a> {
    pub source_id: &'a str,
    pub epoch_id: &'a str,
    pub checkpoint_key: &'a str,
    pub file_identity: &'a str,
    pub byte_offset: u64,
    pub ordinal: u64,
    pub current_turn_id: Option<&'a str>,
    pub events: &'a [NormalizedEvent],
}

struct TurnUpdate<'a> {
    turn_id: &'a str,
    status: &'a str,
    started: i64,
    completed: Option<i64>,
    durable_started: bool,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create database directory {}", parent.display()))?;
        }
        let database = Self {
            path: path.to_path_buf(),
        };
        let _ = database.connect()?;
        Ok(database)
    }

    pub fn connect(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)
            .with_context(|| format!("open database {}", self.path.display()))?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Ok(connection)
    }

    pub fn migrate(&self) -> Result<()> {
        let connection = self.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 0 {
            connection
                .execute_batch(MIGRATION_1)
                .context("apply migration 0001")?;
            connection
                .execute_batch(MIGRATION_2)
                .context("apply migration 0002")?;
            connection
                .execute_batch(MIGRATION_3)
                .context("apply migration 0003")?;
        } else if version == 1 {
            connection
                .execute_batch(MIGRATION_2)
                .context("apply migration 0002")?;
            connection
                .execute_batch(MIGRATION_3)
                .context("apply migration 0003")?;
        } else if version == 2 {
            connection
                .execute_batch(MIGRATION_3)
                .context("apply migration 0003")?;
        } else if version != 3 {
            anyhow::bail!("unsupported observer database schema version {version}");
        }
        connection.execute_batch("PRAGMA integrity_check;")?;
        Ok(())
    }

    pub fn upsert_source(
        &self,
        source_id: &str,
        stable_identity: &str,
        config: &Value,
        status: &str,
    ) -> Result<()> {
        let now = now_ms();
        self.connect()?.execute(
            "INSERT INTO sources(source_id, kind, stable_identity, config_json, status, last_seen_at_ms, created_at_ms, updated_at_ms)
             VALUES (?1, 'rollout', ?2, ?3, ?4, ?5, ?5, ?5)
             ON CONFLICT(source_id) DO UPDATE SET config_json=excluded.config_json, status=excluded.status,
                last_seen_at_ms=excluded.last_seen_at_ms, updated_at_ms=excluded.updated_at_ms",
            params![source_id, stable_identity, config.to_string(), status, now],
        )?;
        Ok(())
    }

    pub fn checkpoint(&self, key: &str) -> Result<Checkpoint> {
        let connection = self.connect()?;
        connection
            .query_row(
                "SELECT byte_offset, ordinal, epoch_id, file_identity, current_turn_id FROM source_checkpoints WHERE checkpoint_key=?1",
                [key],
                |row| {
                    Ok(Checkpoint {
                        byte_offset: row.get::<_, i64>(0)? as u64,
                        ordinal: row.get::<_, i64>(1)? as u64,
                        epoch_id: row.get(2)?,
                        file_identity: row.get(3)?,
                        current_turn_id: row.get(4)?,
                    })
                },
            )
            .optional()
            .map(|value| value.unwrap_or_default())
            .map_err(Into::into)
    }

    pub fn ingest_batch(&self, batch: IngestBatch<'_>) -> Result<(usize, usize)> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO source_epochs(source_id, epoch_id, opened_at_ms) VALUES (?1, ?2, ?3)",
            params![batch.source_id, batch.epoch_id, now_ms()],
        )?;

        let mut inserted = 0;
        let mut deduplicated = 0;
        let mut last_event_seq: Option<i64> = None;
        for event in batch.events {
            let existing = transaction
                .query_row(
                    "SELECT stored_raw_hash,original_event_seq FROM event_dedupes WHERE dedupe_key=?1",
                    [&event.dedupe_key],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            if let Some((existing_hash, original_event_seq)) = existing {
                if existing_hash != event.stored_raw_hash {
                    anyhow::bail!(
                        "dedupe integrity conflict at source ordinal {}",
                        event.source_seq
                    );
                }
                last_event_seq = Some(original_event_seq);
                deduplicated += 1;
                continue;
            }

            transaction.execute(
                "INSERT INTO raw_events(
                   event_id,source_id,epoch_id,source_seq,dedupe_key,observed_at_ms,event_at_ms,
                   thread_key,codex_thread_id,turn_id,item_id,method,phase,durability,
                   source_fingerprint,stored_raw_hash,raw_json,redaction_json,decode_status,decode_error)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'durable',?14,?15,?16,?17,?18,?19)",
                params![
                    event.event_id, event.source_id, event.epoch_id, event.source_seq,
                    event.dedupe_key, event.observed_at_ms, event.event_at_ms,
                    event.thread_key, event.codex_thread_id, event.turn_id, event.item_id,
                    event.method, event.phase, event.source_fingerprint, event.stored_raw_hash,
                    event.raw_json, event.redaction_json, event.decode_status, event.decode_error,
                ],
            )?;
            let event_seq = transaction.last_insert_rowid();
            transaction.execute(
                "INSERT INTO event_dedupes(dedupe_key,stored_raw_hash,original_event_seq,source_id,retained)
                 VALUES (?1,?2,?3,?4,1)",
                params![event.dedupe_key, event.stored_raw_hash, event_seq, event.source_id],
            )?;
            last_event_seq = Some(event_seq);
            inserted += 1;
            project_event(&transaction, event, event_seq)?;
        }

        transaction.execute(
            "INSERT INTO source_checkpoints(checkpoint_key,source_id,epoch_id,file_identity,byte_offset,ordinal,current_turn_id,updated_event_seq,updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(checkpoint_key) DO UPDATE SET epoch_id=excluded.epoch_id,file_identity=excluded.file_identity,
               byte_offset=excluded.byte_offset,ordinal=excluded.ordinal,current_turn_id=excluded.current_turn_id,
               updated_event_seq=COALESCE(excluded.updated_event_seq,source_checkpoints.updated_event_seq),updated_at_ms=excluded.updated_at_ms",
            params![batch.checkpoint_key, batch.source_id, batch.epoch_id, batch.file_identity,
                    batch.byte_offset as i64, batch.ordinal as i64, batch.current_turn_id,
                    last_event_seq, now_ms()],
        )?;
        transaction.commit()?;
        Ok((inserted, deduplicated))
    }

    pub fn mark_location(
        &self,
        source_id: &str,
        thread_id: &str,
        path: &Path,
        representation: &str,
        identity: &str,
        archived: bool,
    ) -> Result<()> {
        self.connect()?.execute(
            "INSERT INTO rollout_locations(store_source_id,codex_thread_id,path,representation,file_identity,active,archived,last_seen_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(store_source_id,codex_thread_id,path) DO UPDATE SET representation=excluded.representation,
               file_identity=excluded.file_identity,active=excluded.active,archived=excluded.archived,last_seen_at_ms=excluded.last_seen_at_ms",
            params![source_id, thread_id, path.display().to_string(), representation, identity, !archived, archived, now_ms()],
        )?;
        self.connect()?.execute(
            "UPDATE threads SET archived=?1 WHERE store_source_id=?2 AND codex_thread_id=?3",
            params![archived, source_id, thread_id],
        )?;
        Ok(())
    }

    pub fn max_event_seq(&self) -> Result<i64> {
        Ok(self.connect()?.query_row(
            "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='raw_events'),0)",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn retention_low_watermark(&self) -> Result<i64> {
        Ok(self.connect()?.query_row(
            "SELECT COALESCE(value_integer,0) FROM retention_state WHERE key='raw_low_watermark'",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn run_retention(&self, retention_days: u64, apply: bool) -> Result<RetentionReport> {
        let retention_ms = retention_days
            .checked_mul(86_400_000)
            .context("raw retention duration overflow")? as i64;
        let cutoff = now_ms().saturating_sub(retention_ms);
        let mut connection = self.connect()?;
        let low_before: i64 = connection.query_row(
            "SELECT COALESCE(value_integer,0) FROM retention_state WHERE key='raw_low_watermark'",
            [],
            |row| row.get(0),
        )?;
        let (candidates, max_candidate): (i64, Option<i64>) = connection.query_row(
            "SELECT COUNT(*),MAX(event_seq) FROM raw_events WHERE observed_at_ms < ?1",
            [cutoff],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if !apply || candidates == 0 {
            return Ok(RetentionReport {
                applied: apply,
                cutoff_at_ms: cutoff,
                candidate_raw_events: candidates as usize,
                deleted_raw_events: 0,
                low_watermark_before: low_before,
                low_watermark_after: low_before,
                dedupe_tombstones_retained: 0,
            });
        }

        let transaction = connection.transaction()?;
        let tombstones = transaction.execute(
            "UPDATE event_dedupes SET retained=0 WHERE retained=1 AND original_event_seq IN
             (SELECT event_seq FROM raw_events WHERE observed_at_ms < ?1)",
            [cutoff],
        )?;
        let deleted =
            transaction.execute("DELETE FROM raw_events WHERE observed_at_ms < ?1", [cutoff])?;
        let low_after = low_before.max(max_candidate.unwrap_or(low_before));
        transaction.execute(
            "INSERT INTO retention_state(key,value_integer,updated_at_ms)
             VALUES ('raw_low_watermark',?1,?2)
             ON CONFLICT(key) DO UPDATE SET value_integer=MAX(retention_state.value_integer,excluded.value_integer),
               updated_at_ms=excluded.updated_at_ms",
            params![low_after, now_ms()],
        )?;
        transaction.commit()?;
        Ok(RetentionReport {
            applied: true,
            cutoff_at_ms: cutoff,
            candidate_raw_events: candidates as usize,
            deleted_raw_events: deleted,
            low_watermark_before: low_before,
            low_watermark_after: low_after,
            dedupe_tombstones_retained: tombstones,
        })
    }

    pub fn doctor(&self, config: &Config) -> Result<DoctorReport> {
        let connection = self.connect()?;
        let integrity: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        let mut degraded = integrity != "ok";
        let sources = config
            .sources
            .iter()
            .map(|source| {
                let exists = source.codex_home.is_dir();
                degraded |= !exists;
                DoctorSource {
                    name: source.name.clone(),
                    path: source.codex_home.display().to_string(),
                    status: if exists {
                        "readable".into()
                    } else {
                        "missing".into()
                    },
                }
            })
            .collect();
        Ok(DoctorReport {
            status: if degraded {
                "degraded".into()
            } else {
                "healthy".into()
            },
            database: integrity,
            sources,
        })
    }

    pub fn rebuild_projections(&self) -> Result<usize> {
        let low_watermark = self.retention_low_watermark()?;
        if low_watermark > 0 {
            anyhow::bail!(
                "projection rebuild is unsafe after raw retention (low watermark {low_watermark})"
            );
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "DELETE FROM search_index; DELETE FROM items; DELETE FROM turns; DELETE FROM threads;",
        )?;
        let events = {
            let mut statement = transaction.prepare(
                "SELECT event_seq,event_id,source_id,epoch_id,source_seq,dedupe_key,observed_at_ms,event_at_ms,
                 thread_key,codex_thread_id,turn_id,item_id,method,phase,source_fingerprint,stored_raw_hash,
                 raw_json,redaction_json,decode_status,decode_error FROM raw_events ORDER BY event_seq"
            )?;
            let rows = statement.query_map([], |row| {
                let raw_json: String = row.get(16)?;
                let raw: Value = serde_json::from_str(&raw_json).unwrap_or(Value::Null);
                let top_type = raw
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let payload = raw.get("payload").cloned().unwrap_or(Value::Null);
                Ok((
                    row.get::<_, i64>(0)?,
                    NormalizedEvent {
                        event_id: row.get(1)?,
                        source_id: row.get(2)?,
                        epoch_id: row.get(3)?,
                        source_seq: row.get(4)?,
                        dedupe_key: row.get(5)?,
                        observed_at_ms: row.get(6)?,
                        event_at_ms: row.get(7)?,
                        thread_key: row.get(8)?,
                        codex_thread_id: row.get(9)?,
                        turn_id: row.get(10)?,
                        item_id: row.get(11)?,
                        method: row.get(12)?,
                        phase: row.get(13)?,
                        source_fingerprint: row.get(14)?,
                        stored_raw_hash: row.get(15)?,
                        raw_json,
                        redaction_json: row.get(17)?,
                        decode_status: row.get(18)?,
                        decode_error: row.get(19)?,
                        top_type,
                        item_type: classify_item(&raw),
                        item_status: Some("completed".into()),
                        summary_text: summary_text(&raw),
                        payload,
                    },
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (seq, event) in &events {
            project_event(&transaction, event, *seq)?;
        }
        transaction.commit()?;
        Ok(events.len())
    }

    pub fn query_json(
        &self,
        sql: &str,
        parameters: &[&dyn rusqlite::ToSql],
        mapper: fn(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
    ) -> Result<Vec<Value>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(sql)?;
        Ok(statement
            .query_map(parameters, mapper)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn query_json_owned(
        &self,
        sql: &str,
        parameters: Vec<SqlValue>,
        mapper: fn(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
    ) -> Result<Vec<Value>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(sql)?;
        Ok(statement
            .query_map(rusqlite::params_from_iter(parameters), mapper)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn project_event(
    transaction: &Transaction<'_>,
    event: &NormalizedEvent,
    event_seq: i64,
) -> Result<()> {
    let time = event.event_at_ms.unwrap_or(event.observed_at_ms);
    let projection = event.payload.to_string();
    transaction.execute(
        "INSERT INTO threads(thread_key,store_source_id,codex_thread_id,capture_completeness,
           completeness_reasons_json,created_at_ms,updated_at_ms,recency_at_ms,projection_json,provenance_json,last_event_seq)
         VALUES (?1,?2,?3,'metadata_only','[]',?4,?4,?4,'{}',?5,?6)
         ON CONFLICT(thread_key) DO UPDATE SET updated_at_ms=MAX(COALESCE(threads.updated_at_ms,0),excluded.updated_at_ms),
           recency_at_ms=MAX(COALESCE(threads.recency_at_ms,0),excluded.recency_at_ms),last_event_seq=MAX(threads.last_event_seq,excluded.last_event_seq)",
        params![event.thread_key, event.source_id, event.codex_thread_id, time,
                json!({"lastEventSeq":event_seq,"sourceId":event.source_id}).to_string(), event_seq],
    )?;

    match event.top_type.as_str() {
        "session_meta" => {
            let p = &event.payload;
            transaction.execute(
                "UPDATE threads SET session_id=?1,cwd=?2,source=?3,model=?4,created_at_ms=COALESCE(?5,created_at_ms),
                   projection_json=?6,provenance_json=?7,last_event_seq=?8 WHERE thread_key=?9",
                params![
                    p.get("session_id").and_then(Value::as_str), p.get("cwd").and_then(Value::as_str),
                    value_as_string(p.get("source")), p.get("model_provider").and_then(Value::as_str),
                    p.get("timestamp").and_then(Value::as_str).and_then(parse_time_ms), projection,
                    json!({"sessionMeta":{"eventSeq":event_seq,"source":"rollout"}}).to_string(), event_seq, event.thread_key,
                ],
            )?;
        }
        "turn_context" => {
            if let Some(turn_id) = event.turn_id.as_deref() {
                upsert_turn(
                    transaction,
                    event,
                    event_seq,
                    TurnUpdate {
                        turn_id,
                        status: "unknown",
                        started: time,
                        completed: None,
                        durable_started: false,
                    },
                )?;
            }
            transaction.execute(
                "UPDATE threads SET cwd=COALESCE(?1,cwd),model=COALESCE(?2,model),projection_json=?3 WHERE thread_key=?4",
                params![event.payload.get("cwd").and_then(Value::as_str), event.payload.get("model").and_then(Value::as_str), projection, event.thread_key],
            )?;
        }
        "event_msg" => {
            let nested_type = event
                .payload
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if nested_type == "task_started" || nested_type == "turn_started" {
                if let Some(turn_id) = event.turn_id.as_deref() {
                    let started = event
                        .payload
                        .get("started_at")
                        .and_then(Value::as_i64)
                        .map(|v| v * 1000)
                        .unwrap_or(time);
                    upsert_turn(
                        transaction,
                        event,
                        event_seq,
                        TurnUpdate {
                            turn_id,
                            status: "running",
                            started,
                            completed: None,
                            durable_started: true,
                        },
                    )?;
                }
            } else if (nested_type == "task_complete" || nested_type == "turn_complete")
                && let Some(turn_id) = event.turn_id.as_deref()
            {
                let completed = event
                    .payload
                    .get("completed_at")
                    .and_then(Value::as_i64)
                    .map(|v| v * 1000)
                    .unwrap_or(time);
                upsert_turn(
                    transaction,
                    event,
                    event_seq,
                    TurnUpdate {
                        turn_id,
                        status: if event.payload.get("error").is_some_and(|v| !v.is_null()) {
                            "failed"
                        } else {
                            "completed"
                        },
                        started: event
                            .payload
                            .get("started_at")
                            .and_then(Value::as_i64)
                            .map(|v| v * 1000)
                            .unwrap_or(time),
                        completed: Some(completed),
                        durable_started: true,
                    },
                )?;
                transaction.execute(
                        "UPDATE threads SET capture_completeness='durable_complete',completeness_reasons_json='[]',runtime_status='idle' WHERE thread_key=?1",
                        [&event.thread_key],
                    )?;
            }
        }
        _ => {}
    }

    if event.top_type != "session_meta" {
        transaction.execute(
            "UPDATE threads SET capture_completeness=CASE WHEN capture_completeness='durable_complete' THEN capture_completeness ELSE 'durable_partial' END,
               completeness_reasons_json=CASE WHEN capture_completeness='durable_complete' THEN completeness_reasons_json ELSE '[\"durable_rollout_may_still_be_growing\"]' END
             WHERE thread_key=?1",
            [&event.thread_key],
        )?;
    }

    if let Some(item_type) = event.item_type.as_deref() {
        let item_id = event.item_id.as_deref().unwrap_or(&event.event_id);
        let turn_scope = event
            .turn_id
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| format!("@unassigned:{}:{}", event.source_id, event.epoch_id));
        let status = event.item_status.as_deref().unwrap_or("completed");
        transaction.execute(
            "INSERT INTO items(thread_key,turn_scope,item_id,turn_id,item_type,status,started_at_ms,completed_at_ms,
               summary_text,projection_json,provenance_json,last_event_seq)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(thread_key,turn_scope,item_id) DO UPDATE SET status=excluded.status,
               completed_at_ms=COALESCE(excluded.completed_at_ms,items.completed_at_ms),
               summary_text=COALESCE(excluded.summary_text,items.summary_text),projection_json=excluded.projection_json,
               provenance_json=excluded.provenance_json,last_event_seq=excluded.last_event_seq",
            params![event.thread_key, turn_scope, item_id, event.turn_id, item_type, status, time,
                    if status == "completed" || status == "failed" { Some(time) } else { None }, event.summary_text,
                    projection, json!({"eventSeq":event_seq,"source":"rollout"}).to_string(), event_seq],
        )?;
        if let Some(summary) = event
            .summary_text
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            let entity_key = format!("{}:{}:{}", event.thread_key, turn_scope, item_id);
            transaction.execute(
                "DELETE FROM search_index WHERE entity_key=?1",
                [&entity_key],
            )?;
            transaction.execute(
                "INSERT INTO search_index(entity_key,thread_key,item_id,text) VALUES (?1,?2,?3,?4)",
                params![entity_key, event.thread_key, item_id, summary],
            )?;
            transaction.execute(
                "UPDATE threads SET last_message_preview=?1 WHERE thread_key=?2",
                params![truncate(summary, 240), event.thread_key],
            )?;
        }
    }
    Ok(())
}

fn upsert_turn(
    tx: &Transaction<'_>,
    event: &NormalizedEvent,
    seq: i64,
    update: TurnUpdate<'_>,
) -> Result<()> {
    let terminal = matches!(update.status, "completed" | "failed" | "interrupted");
    let completeness = if terminal {
        "durable_complete"
    } else {
        "durable_partial"
    };
    let reasons = if terminal {
        "[]"
    } else {
        "[\"durable_turn_not_terminal\"]"
    };
    let coverage = json!({
        "liveStarted":false,"liveTerminal":false,"liveEpochContiguous":false,
        "durableStarted":update.durable_started,"durableTerminal":terminal,"durableEofReached":terminal,
        "decodeErrorCount":0,"unknownEventCount":0,"sourceDisconnectCount":0
    });
    tx.execute(
        "INSERT INTO turns(thread_key,turn_id,status,capture_completeness,completeness_reasons_json,coverage_json,
           started_at_ms,completed_at_ms,execution_context_json,projection_json,provenance_json,last_event_seq)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(thread_key,turn_id) DO UPDATE SET status=excluded.status,
           capture_completeness=excluded.capture_completeness,completeness_reasons_json=excluded.completeness_reasons_json,
           coverage_json=excluded.coverage_json,started_at_ms=MIN(COALESCE(turns.started_at_ms,excluded.started_at_ms),excluded.started_at_ms),
           completed_at_ms=COALESCE(excluded.completed_at_ms,turns.completed_at_ms),
           execution_context_json=COALESCE(excluded.execution_context_json,turns.execution_context_json),
           projection_json=excluded.projection_json,provenance_json=excluded.provenance_json,last_event_seq=excluded.last_event_seq",
        params![event.thread_key, update.turn_id, update.status, completeness, reasons, coverage.to_string(), update.started, update.completed,
                if event.top_type == "turn_context" { Some(event.payload.to_string()) } else { None },
                event.payload.to_string(), json!({"eventSeq":seq}).to_string(), seq],
    )?;
    Ok(())
}

pub fn classify_item(raw: &Value) -> Option<String> {
    let top = raw.get("type")?.as_str()?;
    let payload = raw.get("payload")?;
    let kind = payload.get("type").and_then(Value::as_str).unwrap_or(top);
    match top {
        "response_item" => Some(
            match kind {
                "message" => match payload.get("role").and_then(Value::as_str) {
                    Some("user") => "user_message",
                    _ => "agent_message",
                },
                "reasoning" => "reasoning",
                "local_shell_call" => "command_execution",
                "function_call" | "custom_tool_call" | "tool_search_call" => "tool_call",
                "function_call_output" | "custom_tool_call_output" => "tool_output",
                other => other,
            }
            .to_string(),
        ),
        "compacted" => Some("reasoning".into()),
        "event_msg" => match kind {
            "user_message" => Some("user_message".into()),
            "agent_message" => Some("agent_message".into()),
            "agent_reasoning" => Some("reasoning".into()),
            "exec_command_begin" | "exec_command_end" | "exec_command_output_delta" => {
                Some("command_execution".into())
            }
            "mcp_tool_call_begin" | "mcp_tool_call_end" => Some("mcp_tool_call".into()),
            "turn_diff" | "patch_apply_begin" | "patch_apply_end" => Some("file_change".into()),
            "plan_update" => Some("plan".into()),
            "error" | "warning" | "stream_error" => Some("error".into()),
            "token_count" => Some("usage".into()),
            _ => None,
        },
        other if !matches!(other, "session_meta" | "turn_context" | "world_state") => {
            Some("unknown".into())
        }
        _ => None,
    }
}

pub fn summary_text(raw: &Value) -> Option<String> {
    let payload = raw.get("payload")?;
    if let Some(text) = payload
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| payload.get("text").and_then(Value::as_str))
        .or_else(|| payload.get("name").and_then(Value::as_str))
        .or_else(|| payload.get("call_id").and_then(Value::as_str))
    {
        return Some(truncate(text, 16_000));
    }
    if let Some(content) = payload.get("content").and_then(Value::as_array) {
        let joined = content
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("input_text").and_then(Value::as_str))
                    .or_else(|| item.get("output_text").and_then(Value::as_str))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !joined.is_empty() {
            return Some(truncate(&joined, 16_000));
        }
    }
    if let Some(summary) = payload.get("summary").and_then(Value::as_array) {
        let joined = summary
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if !joined.is_empty() {
            return Some(truncate(&joined, 16_000));
        }
    }
    None
}

fn value_as_string(value: Option<&Value>) -> Option<String> {
    value.map(|value| {
        value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string())
    })
}

fn parse_time_ms(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp_millis())
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}
