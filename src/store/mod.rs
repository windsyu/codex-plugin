use std::collections::HashSet;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::{Context, Result};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

use crate::clock::now_ms;
use crate::config::Config;
use crate::domain::classify::{classify_item, summary_text};
use crate::domain::identity::thread_key;
use crate::domain::live::{classify_live_item, live_summary};
use crate::domain::model::{
    Checkpoint, CoverageFlags, DoctorReport, DoctorSource, ExportReport, NormalizedEvent,
    OwnedIngestBatch, PurgeReport, RetentionReport,
};
use crate::domain::normalize::parse_time_ms;
use crate::domain::project::{inferred_project_key, project_name};
use crate::permissions::{
    create_private_file, prepare_database_files, prepare_private_dir, prepare_private_file,
};

mod blob;
mod maintenance;
mod pool;
mod projection;
mod query;
mod schema;
mod write;

pub use blob::BlobRecord;
use blob::PreparedBlob;
use maintenance::{read_only_mode, write_private_new};
use pool::{ReadConnection, ReadPool};
use projection::{projection_reference_key, truncate};
use query::query_json_connection;
pub use schema::LATEST_SCHEMA_VERSION;
#[cfg(test)]
use schema::*;
use write::TurnUpdate;

struct SessionContextBackfill {
    base_instructions: Option<String>,
    dynamic_tools: Option<String>,
    selected_capability_roots: Option<String>,
    memory_mode: Option<String>,
    subagent_history_start_ordinal: Option<i64>,
    multi_agent_version: Option<String>,
    context_window: Option<String>,
}

fn session_context_from_raw(raw_json: &str) -> Option<SessionContextBackfill> {
    let value: Value = serde_json::from_str(raw_json).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let payload = value.get("payload")?;
    Some(SessionContextBackfill {
        base_instructions: payload
            .get("base_instructions")
            .filter(|value| !value.is_null())
            .map(Value::to_string),
        dynamic_tools: payload
            .get("dynamic_tools")
            .filter(|value| !value.is_null())
            .map(Value::to_string),
        selected_capability_roots: payload
            .get("selected_capability_roots")
            .filter(|value| !value.is_null())
            .map(Value::to_string),
        memory_mode: payload
            .get("memory_mode")
            .and_then(Value::as_str)
            .map(str::to_string),
        subagent_history_start_ordinal: payload
            .get("subagent_history_start_ordinal")
            .and_then(Value::as_i64),
        multi_agent_version: payload
            .get("multi_agent_version")
            .filter(|value| !value.is_null())
            .map(Value::to_string),
        context_window: payload
            .get("context_window")
            .filter(|value| !value.is_null())
            .map(Value::to_string),
    })
}

pub struct Database {
    path: PathBuf,
    blob_dir: PathBuf,
    inline_blob_bytes: usize,
    read_pool: ReadPool,
    #[cfg(test)]
    fail_before_commit: AtomicU8,
}

impl Database {
    #[cfg(test)]
    pub fn open(path: &Path) -> Result<Self> {
        let blob_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("blobs");
        Self::open_with_blobs(path, &blob_dir, 256 * 1024)
    }

    pub fn open_with_blobs(path: &Path, blob_dir: &Path, inline_blob_bytes: usize) -> Result<Self> {
        if let Some(parent) = path.parent() {
            prepare_private_dir(parent, "Observer data")?;
        }
        if fs::symlink_metadata(path).is_ok() {
            prepare_private_file(path, "Observer database")?;
        } else {
            drop(create_private_file(path, "Observer database")?);
        }
        let database = Self {
            path: path.to_path_buf(),
            blob_dir: blob_dir.to_path_buf(),
            inline_blob_bytes,
            read_pool: ReadPool::new(path.to_path_buf(), 8),
            #[cfg(test)]
            fail_before_commit: AtomicU8::new(0),
        };
        database.initialize_blob_dir()?;
        let _ = database.connect()?;
        prepare_database_files(path)?;
        Ok(database)
    }

    pub fn open_read_only_with_blobs(
        path: &Path,
        blob_dir: &Path,
        inline_blob_bytes: usize,
    ) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| "Observer database does not exist or is not readable")?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            anyhow::bail!("Observer database must be a direct regular file");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() } {
                anyhow::bail!("Observer database must be owned by the current user");
            }
        }
        let database = Self {
            path: path.to_path_buf(),
            blob_dir: blob_dir.to_path_buf(),
            inline_blob_bytes,
            read_pool: ReadPool::new(path.to_path_buf(), 8),
            #[cfg(test)]
            fail_before_commit: AtomicU8::new(0),
        };
        let connection = database.connect_read_only()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version != LATEST_SCHEMA_VERSION {
            anyhow::bail!(
                "Observer export requires schema version {LATEST_SCHEMA_VERSION}; found {version}"
            );
        }
        Ok(database)
    }

    pub fn connect(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)
            .with_context(|| format!("open database {}", self.path.display()))?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Ok(connection)
    }

    pub fn connect_read_only(&self) -> Result<Connection> {
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .with_context(|| "open Observer database read-only")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "query_only", true)?;
        Ok(connection)
    }

    fn read_connection(&self) -> Result<ReadConnection<'_>> {
        self.read_pool.get()
    }

    fn initialize_blob_dir(&self) -> Result<()> {
        prepare_private_dir(&self.blob_dir, "blob")
    }

    fn prepare_blob(&self, event: &NormalizedEvent) -> Result<Option<PreparedBlob>> {
        let bytes = event.raw_json.as_bytes();
        if bytes.len() <= self.inline_blob_bytes {
            return Ok(None);
        }
        let stored_hash = event.stored_raw_hash.clone();
        let blob_id = format!("blob_{stored_hash}");
        let relative_path = format!("{}/{}.json", &stored_hash[..2], stored_hash);
        let final_path = self.blob_dir.join(&relative_path);
        let parent = final_path.parent().context("blob path has no parent")?;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(parent)?;
        prepare_private_dir(parent, "blob shard")?;
        if !final_path.exists() {
            let temp_path = parent.join(format!(".{stored_hash}.{}.tmp", uuid::Uuid::new_v4()));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temp_path)
                .with_context(|| format!("create blob temp file {}", temp_path.display()))?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            if let Err(error) = fs::rename(&temp_path, &final_path) {
                let _ = fs::remove_file(&temp_path);
                return Err(error).context("atomically publish blob");
            }
            fs::File::open(parent)?.sync_all()?;
        }
        let metadata = fs::symlink_metadata(&final_path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() != bytes.len() as u64
        {
            anyhow::bail!("existing content-addressed blob does not match expected file");
        }
        prepare_private_file(&final_path, "Observer blob")?;
        let mut verified = Vec::with_capacity(bytes.len());
        self.open_blob_path(&relative_path)?
            .read_to_end(&mut verified)?;
        if blake3::hash(&verified).to_hex().as_str() != stored_hash {
            anyhow::bail!("content-addressed blob hash verification failed");
        }
        Ok(Some(PreparedBlob {
            blob_id,
            stored_hash,
            media_type: "application/json",
            size_bytes: bytes.len(),
            relative_path,
            redaction_json: event.redaction_json.clone(),
        }))
    }

    fn resolve_blob_path(&self, relative_path: &str) -> Result<PathBuf> {
        let relative = Path::new(relative_path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            anyhow::bail!("invalid stored blob relative path");
        }
        Ok(self.blob_dir.join(relative))
    }

    fn open_blob_path(&self, relative_path: &str) -> Result<fs::File> {
        let path = self.resolve_blob_path(relative_path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("open blob {}", path.display()))?;
        if !file.metadata()?.is_file() {
            anyhow::bail!("stored blob is not a regular file");
        }
        Ok(file)
    }

    fn read_blob_relative(&self, relative_path: &str) -> Result<String> {
        let mut contents = String::new();
        self.open_blob_path(relative_path)?
            .read_to_string(&mut contents)?;
        Ok(contents)
    }

    pub fn blob_record(&self, blob_id: &str) -> Result<Option<BlobRecord>> {
        let record = self
            .read_connection()?
            .query_row(
                "SELECT b.blob_id,b.media_type,b.size_bytes,b.relative_path
                 FROM blobs b WHERE b.blob_id=?1
                   AND EXISTS (SELECT 1 FROM blob_references r WHERE r.blob_id=b.blob_id)",
                [blob_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        record
            .map(|(blob_id, media_type, size_bytes, relative_path)| {
                Ok(BlobRecord {
                    blob_id,
                    media_type,
                    size_bytes: size_bytes as u64,
                    path: self.resolve_blob_path(&relative_path)?,
                })
            })
            .transpose()
    }

    pub fn open_blob(&self, record: &BlobRecord) -> Result<fs::File> {
        let relative = record
            .path
            .strip_prefix(&self.blob_dir)
            .context("blob record escaped blob_dir")?;
        let file = self.open_blob_path(&relative.to_string_lossy())?;
        if file.metadata()?.len() != record.size_bytes {
            anyhow::bail!("blob file size does not match metadata");
        }
        Ok(file)
    }

    pub fn sweep_orphan_blobs(&self, grace_ms: u64) -> Result<usize> {
        let connection = self.read_connection()?;
        let cutoff = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_millis(grace_ms))
            .unwrap_or(std::time::UNIX_EPOCH);
        let mut deleted = 0;
        for entry in walkdir::WalkDir::new(&self.blob_dir)
            .follow_links(false)
            .min_depth(1)
        {
            let entry = entry?;
            if !entry.file_type().is_file() || entry.metadata()?.modified()? > cutoff {
                continue;
            }
            let relative = entry.path().strip_prefix(&self.blob_dir)?;
            let relative = relative.to_string_lossy();
            let referenced: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM blobs WHERE relative_path=?1)",
                [relative.as_ref()],
                |row| row.get(0),
            )?;
            if !referenced {
                fs::remove_file(entry.path())?;
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    pub fn migrate(&self) -> Result<()> {
        let mut connection = self.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if !(0..=LATEST_SCHEMA_VERSION).contains(&version) {
            anyhow::bail!("unsupported observer database schema version {version}");
        }
        for (target, migration) in schema::migrations() {
            if version < target {
                connection
                    .execute_batch(migration)
                    .with_context(|| format!("apply migration {target:04}"))?;
            }
        }
        Self::backfill_project_and_context(&mut connection)?;
        recompute_all_completeness(&connection)?;
        connection.execute_batch("PRAGMA integrity_check;")?;
        prepare_database_files(&self.path)?;
        Ok(())
    }

    fn backfill_project_and_context(connection: &mut Connection) -> Result<()> {
        let transaction = connection.transaction()?;
        let thread_rows = {
            let mut statement =
                transaction.prepare("SELECT thread_key,cwd,project_key,originator FROM threads")?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (thread_key_value, cwd, existing_project_key, originator) in thread_rows {
            if existing_project_key.is_none()
                && let Some(cwd_value) = cwd.as_deref()
                && let Some(key) = inferred_project_key(cwd_value, originator.as_deref())
            {
                transaction.execute(
                    "UPDATE threads SET project_key=?1 WHERE thread_key=?2 AND project_key IS NULL",
                    params![key, thread_key_value],
                )?;
            }

            let raw_json: Option<String> = transaction
                .query_row(
                    "SELECT raw_json FROM raw_events WHERE thread_key=?1 AND method='rollout/session_meta'
                     ORDER BY event_seq DESC LIMIT 1",
                    [&thread_key_value],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            let Some(raw_json) = raw_json else {
                continue;
            };
            let Some(context) = session_context_from_raw(&raw_json) else {
                continue;
            };
            transaction.execute(
                "UPDATE threads SET
                   base_instructions_json=COALESCE(base_instructions_json,?1),
                   dynamic_tools_json=COALESCE(dynamic_tools_json,?2),
                   selected_capability_roots_json=COALESCE(selected_capability_roots_json,?3),
                   memory_mode=COALESCE(memory_mode,?4),
                   subagent_history_start_ordinal=COALESCE(subagent_history_start_ordinal,?5),
                   multi_agent_version=COALESCE(multi_agent_version,?6),
                   context_window_json=COALESCE(context_window_json,?7)
                 WHERE thread_key=?8",
                params![
                    context.base_instructions,
                    context.dynamic_tools,
                    context.selected_capability_roots,
                    context.memory_mode,
                    context.subagent_history_start_ordinal,
                    context.multi_agent_version,
                    context.context_window,
                    thread_key_value,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn upsert_source(
        &self,
        source_id: &str,
        stable_identity: &str,
        config: &Value,
        status: &str,
    ) -> Result<()> {
        self.upsert_source_kind(source_id, "rollout", stable_identity, config, status)
    }

    pub fn upsert_source_kind(
        &self,
        source_id: &str,
        kind: &str,
        stable_identity: &str,
        config: &Value,
        status: &str,
    ) -> Result<()> {
        self.upsert_source_kind_on(
            &self.connect()?,
            source_id,
            kind,
            stable_identity,
            config,
            status,
        )
    }

    pub(crate) fn upsert_source_kind_on(
        &self,
        connection: &Connection,
        source_id: &str,
        kind: &str,
        stable_identity: &str,
        config: &Value,
        status: &str,
    ) -> Result<()> {
        let now = now_ms();
        connection.execute(
            "INSERT INTO sources(source_id, kind, stable_identity, config_json, status, last_seen_at_ms, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?6)
             ON CONFLICT(source_id) DO UPDATE SET kind=excluded.kind,config_json=excluded.config_json,status=excluded.status,
                last_seen_at_ms=excluded.last_seen_at_ms, updated_at_ms=excluded.updated_at_ms",
            params![source_id, kind, stable_identity, config.to_string(), status, now],
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

    pub fn ingest_batch(&self, batch: &OwnedIngestBatch) -> Result<(usize, usize)> {
        self.ingest_batches(std::slice::from_ref(batch))?
            .pop()
            .context("ingest group returned no result")
    }

    pub fn ingest_batches(&self, batches: &[OwnedIngestBatch]) -> Result<Vec<(usize, usize)>> {
        let mut connection = self.connect()?;
        self.ingest_batches_on(&mut connection, batches)
    }

    pub(crate) fn ingest_batches_on(
        &self,
        connection: &mut Connection,
        batches: &[OwnedIngestBatch],
    ) -> Result<Vec<(usize, usize)>> {
        let transaction = connection.transaction()?;
        let mut results = Vec::with_capacity(batches.len());
        for batch in batches {
            results.push(self.ingest_batch_transaction(&transaction, batch)?);
        }
        #[cfg(test)]
        match self.fail_before_commit.swap(0, Ordering::SeqCst) {
            1 => anyhow::bail!("injected SQLITE_FULL before commit"),
            2 => anyhow::bail!("injected SQLITE_IOERR before commit"),
            _ => {}
        }
        transaction.commit()?;
        Ok(results)
    }

    #[cfg(test)]
    pub fn fail_next_ingest_for_test(&self, kind: &str) {
        let code = match kind {
            "disk_full" => 1,
            "io_error" => 2,
            _ => panic!("unknown ingest failpoint"),
        };
        self.fail_before_commit.store(code, Ordering::SeqCst);
    }

    fn ingest_batch_transaction(
        &self,
        transaction: &Transaction<'_>,
        batch: &OwnedIngestBatch,
    ) -> Result<(usize, usize)> {
        transaction.execute(
            "INSERT OR IGNORE INTO source_epochs(source_id, epoch_id, opened_at_ms) VALUES (?1, ?2, ?3)",
            params![batch.source_id, batch.epoch_id, now_ms()],
        )?;
        let purged_threads = {
            let mut statement = transaction.prepare("SELECT thread_key FROM purged_threads")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<HashSet<_>>>()?
        };

        let mut inserted = 0;
        let mut deduplicated = 0;
        let mut decode_errors = 0_i64;
        let mut unknown_events = 0_i64;
        let mut sequence_gaps = 0_i64;
        let mut last_source_seq: Option<i64> = transaction.query_row(
            "SELECT last_source_seq FROM source_epochs WHERE source_id=?1 AND epoch_id=?2",
            params![batch.source_id, batch.epoch_id],
            |row| row.get(0),
        )?;
        let mut last_event_seq: Option<i64> = None;
        let mut touched_threads = HashSet::new();
        for event in &batch.events {
            if purged_threads.contains(&event.thread_key) {
                deduplicated += 1;
                continue;
            }
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

            let prepared_blob = self.prepare_blob(event)?;
            let mut stored_event = event.clone();
            if let Some(blob) = prepared_blob.as_ref() {
                let blob_ref = json!({
                    "blobId":blob.blob_id,"size":blob.size_bytes,"mediaType":blob.media_type,
                    "storedHash":blob.stored_hash,"redacted":true
                });
                stored_event.blob_id = Some(blob.blob_id.clone());
                stored_event.raw_json = json!({"blobRef":blob_ref}).to_string();
                if stored_event.item_type.is_some() {
                    stored_event.payload = json!({"blobRefs":[blob_ref]});
                }
            }

            transaction.execute(
                "INSERT INTO raw_events(
                   event_id,source_id,epoch_id,source_seq,dedupe_key,observed_at_ms,event_at_ms,
                   thread_key,codex_thread_id,turn_id,item_id,method,phase,durability,
                   source_fingerprint,stored_raw_hash,raw_json,redaction_json,decode_status,decode_error,request_id,store_source_id,blob_id,
                   protocol_direction,worker_id,worker_connection_epoch,proxy_seq)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)",
                params![
                    stored_event.event_id, stored_event.source_id, stored_event.epoch_id, stored_event.source_seq,
                    stored_event.dedupe_key, stored_event.observed_at_ms, stored_event.event_at_ms,
                    stored_event.thread_key, stored_event.codex_thread_id, stored_event.turn_id, stored_event.item_id,
                    stored_event.method, stored_event.phase, stored_event.durability, stored_event.source_fingerprint, stored_event.stored_raw_hash,
                    stored_event.raw_json, stored_event.redaction_json, stored_event.decode_status, stored_event.decode_error,
                    stored_event.request_id, stored_event.store_source_id, stored_event.blob_id,
                    stored_event.protocol_direction, stored_event.worker_id,
                    stored_event.worker_connection_epoch, stored_event.proxy_seq,
                ],
            )?;
            let event_seq = transaction.last_insert_rowid();
            if let Some(previous) = last_source_seq
                && event.source_seq > previous.saturating_add(1)
            {
                sequence_gaps = sequence_gaps
                    .saturating_add(event.source_seq.saturating_sub(previous).saturating_sub(1));
            }
            last_source_seq = Some(
                last_source_seq.map_or(event.source_seq, |previous| previous.max(event.source_seq)),
            );
            decode_errors += i64::from(event.decode_status == "error");
            unknown_events += i64::from(event.decode_status == "unknown");
            if let Some(blob) = prepared_blob.as_ref() {
                transaction.execute(
                    "INSERT INTO blobs(blob_id,stored_hash,media_type,size_bytes,relative_path,redaction_json,created_event_seq,created_at_ms)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(blob_id) DO NOTHING",
                    params![blob.blob_id,blob.stored_hash,blob.media_type,blob.size_bytes as i64,
                        blob.relative_path,blob.redaction_json,event_seq,now_ms()],
                )?;
                transaction.execute(
                    "INSERT INTO blob_references(reference_kind,reference_key,blob_id,event_seq)
                     VALUES ('raw_event',?1,?2,?3)",
                    params![event_seq.to_string(), blob.blob_id, event_seq],
                )?;
            }
            transaction.execute(
                "INSERT INTO event_dedupes(dedupe_key,stored_raw_hash,original_event_seq,source_id,retained)
                 VALUES (?1,?2,?3,?4,1)",
                params![event.dedupe_key, event.stored_raw_hash, event_seq, event.source_id],
            )?;
            last_event_seq = Some(event_seq);
            inserted += 1;
            if stored_event.projectable {
                project_event(transaction, &stored_event, event_seq)?;
                update_event_coverage(transaction, &stored_event)?;
                touched_threads.insert(stored_event.thread_key.clone());
                let reference_key = projection_reference_key(&stored_event);
                if let Some(blob) = prepared_blob
                    .as_ref()
                    .filter(|_| stored_event.item_type.is_some())
                {
                    transaction.execute(
                        "INSERT INTO blob_references(reference_kind,reference_key,blob_id,event_seq)
                         VALUES ('projection',?1,?2,?3)
                         ON CONFLICT(reference_kind,reference_key) DO UPDATE SET
                           blob_id=excluded.blob_id,event_seq=excluded.event_seq",
                        params![reference_key,blob.blob_id,event_seq],
                    )?;
                } else {
                    transaction.execute(
                        "DELETE FROM blob_references WHERE reference_kind='projection' AND reference_key=?1",
                        [reference_key],
                    )?;
                }
            }
        }

        transaction.execute(
            "UPDATE source_epochs SET event_count=event_count+?1,
               sequence_gap_count=sequence_gap_count+?2,decode_error_count=decode_error_count+?3,
               unknown_event_count=unknown_event_count+?4,last_source_seq=?5,last_event_at_ms=?6
             WHERE source_id=?7 AND epoch_id=?8",
            params![
                inserted as i64,
                sequence_gaps,
                decode_errors,
                unknown_events,
                last_source_seq,
                now_ms(),
                batch.source_id,
                batch.epoch_id
            ],
        )?;

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
        if batch.clean_eof {
            let turn_keys = {
                let mut statement = transaction.prepare(
                    "SELECT DISTINCT thread_key,turn_id FROM raw_events
                     WHERE source_id=?1 AND epoch_id=?2 AND turn_id IS NOT NULL",
                )?;
                statement
                    .query_map(params![batch.source_id, batch.epoch_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (thread_key, turn_id) in turn_keys {
                let mut coverage = load_coverage(transaction, &thread_key, &turn_id)?;
                coverage.durable_eof_reached = true;
                store_coverage(transaction, &thread_key, &turn_id, &coverage)?;
                touched_threads.insert(thread_key);
            }
        }
        for thread_key in touched_threads {
            recompute_thread_completeness(transaction, &thread_key)?;
        }
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
        self.mark_location_on(
            &self.connect()?,
            source_id,
            thread_id,
            path,
            representation,
            identity,
            archived,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mark_location_on(
        &self,
        connection: &Connection,
        source_id: &str,
        thread_id: &str,
        path: &Path,
        representation: &str,
        identity: &str,
        archived: bool,
    ) -> Result<()> {
        let purged: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM purged_threads WHERE store_source_id=?1 AND codex_thread_id=?2)",
            params![source_id, thread_id],
            |row| row.get(0),
        )?;
        if purged {
            return Ok(());
        }
        connection.execute(
            "INSERT INTO rollout_locations(store_source_id,codex_thread_id,path,representation,file_identity,active,archived,last_seen_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(store_source_id,codex_thread_id,path) DO UPDATE SET representation=excluded.representation,
               file_identity=excluded.file_identity,active=excluded.active,archived=excluded.archived,last_seen_at_ms=excluded.last_seen_at_ms",
            params![source_id, thread_id, path.display().to_string(), representation, identity, !archived, archived, now_ms()],
        )?;
        connection.execute(
            "UPDATE threads SET archived=?1 WHERE store_source_id=?2 AND codex_thread_id=?3",
            params![archived, source_id, thread_id],
        )?;
        Ok(())
    }

    pub fn max_event_seq(&self) -> Result<i64> {
        Ok(self.read_connection()?.query_row(
            "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='raw_events'),0)",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn legacy_redaction_event_count(&self) -> Result<i64> {
        Ok(self.read_connection()?.query_row(
            "SELECT COUNT(*) FROM raw_events WHERE redaction_json NOT LIKE '%known-secrets-v2%'",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn retention_low_watermark(&self) -> Result<i64> {
        Ok(self.read_connection()?.query_row(
            "SELECT COALESCE(value_integer,0) FROM retention_state WHERE key='raw_low_watermark'",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn run_retention(
        &self,
        retention_days: u64,
        delta_retention_days: u64,
        blob_retention_days: u64,
        apply: bool,
    ) -> Result<RetentionReport> {
        let retention_ms = retention_days
            .checked_mul(86_400_000)
            .context("raw retention duration overflow")? as i64;
        let cutoff = now_ms().saturating_sub(retention_ms);
        let delta_retention_ms = delta_retention_days
            .checked_mul(86_400_000)
            .context("delta retention duration overflow")? as i64;
        let delta_cutoff = now_ms().saturating_sub(delta_retention_ms);
        let blob_retention_ms = blob_retention_days
            .checked_mul(86_400_000)
            .context("blob retention duration overflow")? as i64;
        let blob_cutoff = now_ms().saturating_sub(blob_retention_ms);
        let mut connection = self.connect()?;
        let low_before: i64 = connection.query_row(
            "SELECT COALESCE(value_integer,0) FROM retention_state WHERE key='raw_low_watermark'",
            [],
            |row| row.get(0),
        )?;
        let (candidates, max_candidate): (i64, Option<i64>) = connection.query_row(
            "SELECT COUNT(*),MAX(event_seq) FROM raw_events WHERE observed_at_ms < ?1
             OR (durability='transient' AND phase='delta' AND observed_at_ms < ?2)",
            params![cutoff, delta_cutoff],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let candidate_blobs: i64 = connection.query_row(
            "SELECT COUNT(*) FROM blobs b WHERE b.created_at_ms < ?1 AND NOT EXISTS (
               SELECT 1 FROM blob_references r WHERE r.blob_id=b.blob_id AND (
                 r.reference_kind='projection' OR (r.reference_kind='raw_event' AND EXISTS (
                   SELECT 1 FROM raw_events e WHERE e.event_seq=r.event_seq AND NOT
                     (e.observed_at_ms<?2 OR (e.durability='transient' AND e.phase='delta' AND e.observed_at_ms<?3))
                 ))
               )
             )",
            params![blob_cutoff, cutoff, delta_cutoff],
            |row| row.get(0),
        )?;
        if !apply || (candidates == 0 && candidate_blobs == 0) {
            return Ok(RetentionReport {
                applied: apply,
                cutoff_at_ms: cutoff,
                candidate_raw_events: candidates as usize,
                deleted_raw_events: 0,
                candidate_blobs: candidate_blobs as usize,
                deleted_blobs: 0,
                low_watermark_before: low_before,
                low_watermark_after: low_before,
                dedupe_tombstones_retained: 0,
            });
        }

        let transaction = connection.transaction()?;
        let tombstones = transaction.execute(
            "UPDATE event_dedupes SET retained=0 WHERE retained=1 AND original_event_seq IN
             (SELECT event_seq FROM raw_events WHERE observed_at_ms < ?1
                OR (durability='transient' AND phase='delta' AND observed_at_ms < ?2))",
            params![cutoff, delta_cutoff],
        )?;
        transaction.execute(
            "DELETE FROM blob_references WHERE reference_kind='raw_event' AND (
               event_seq IN (SELECT event_seq FROM raw_events WHERE observed_at_ms < ?1
                 OR (durability='transient' AND phase='delta' AND observed_at_ms < ?2))
               OR NOT EXISTS (SELECT 1 FROM raw_events e WHERE e.event_seq=blob_references.event_seq)
             )",
            params![cutoff,delta_cutoff],
        )?;
        let deleted = transaction.execute(
            "DELETE FROM raw_events WHERE observed_at_ms < ?1
             OR (durability='transient' AND phase='delta' AND observed_at_ms < ?2)",
            params![cutoff, delta_cutoff],
        )?;
        let blob_paths = {
            let mut statement = transaction.prepare(
                "SELECT relative_path FROM blobs b WHERE b.created_at_ms < ?1
                 AND NOT EXISTS (SELECT 1 FROM blob_references r WHERE r.blob_id=b.blob_id)",
            )?;
            statement
                .query_map([blob_cutoff], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let deleted_blobs = transaction.execute(
            "DELETE FROM blobs WHERE created_at_ms < ?1
             AND NOT EXISTS (SELECT 1 FROM blob_references r WHERE r.blob_id=blobs.blob_id)",
            [blob_cutoff],
        )?;
        let low_after = low_before.max(max_candidate.unwrap_or(low_before));
        transaction.execute(
            "INSERT INTO retention_state(key,value_integer,updated_at_ms)
             VALUES ('raw_low_watermark',?1,?2)
             ON CONFLICT(key) DO UPDATE SET value_integer=MAX(retention_state.value_integer,excluded.value_integer),
               updated_at_ms=excluded.updated_at_ms",
            params![low_after, now_ms()],
        )?;
        transaction.commit()?;
        for relative_path in blob_paths {
            let path = self.resolve_blob_path(&relative_path)?;
            if let Err(error) = fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %path.display(), error = %error, "delete expired blob file failed");
            }
        }
        Ok(RetentionReport {
            applied: true,
            cutoff_at_ms: cutoff,
            candidate_raw_events: candidates as usize,
            deleted_raw_events: deleted,
            candidate_blobs: candidate_blobs as usize,
            deleted_blobs,
            low_watermark_before: low_before,
            low_watermark_after: low_after,
            dedupe_tombstones_retained: tombstones,
        })
    }

    pub fn export_thread(&self, thread_key: &str, output: &Path) -> Result<ExportReport> {
        let connection = self.connect_read_only()?;
        connection.execute_batch("BEGIN")?;
        let thread = connection
            .query_row(
                "SELECT thread_key,codex_thread_id,store_source_id,name,cwd,source,model,archived,
                   capture_completeness,completeness_reasons_json,created_at_ms,updated_at_ms,recency_at_ms,
                   projection_json,provenance_json,last_event_seq,project_key,base_instructions_json,
                   dynamic_tools_json,selected_capability_roots_json,memory_mode,
                   subagent_history_start_ordinal,multi_agent_version,context_window_json
                 FROM threads WHERE thread_key=?1",
                [thread_key],
                |row| {
                    let cwd: Option<String> = row.get(4)?;
                    let project_key_value: Option<String> = row.get(16)?;
                    let project = project_key_value
                        .filter(|value| !value.is_empty())
                        .map(|key| json!({
                            "key":key,"name":project_name(&key),
                            "path":cwd.clone().unwrap_or_else(|| key.clone())
                        }));
                    Ok(json!({
                        "threadKey":row.get::<_,String>(0)?,"codexThreadId":row.get::<_,String>(1)?,
                        "storeSourceId":row.get::<_,String>(2)?,"name":row.get::<_,Option<String>>(3)?,
                        "cwd":cwd,"source":row.get::<_,Option<String>>(5)?,
                        "model":row.get::<_,Option<String>>(6)?,"archived":row.get::<_,bool>(7)?,
                        "captureCompleteness":row.get::<_,String>(8)?,
                        "completenessReasons":parse_stored_json(row.get::<_,String>(9)?),
                        "createdAtMs":row.get::<_,Option<i64>>(10)?,"updatedAtMs":row.get::<_,Option<i64>>(11)?,
                        "recencyAtMs":row.get::<_,Option<i64>>(12)?,
                        "projection":parse_stored_json(row.get::<_,String>(13)?),
                        "provenance":parse_stored_json(row.get::<_,String>(14)?),
                        "lastEventSeq":row.get::<_,i64>(15)?,
                        "project":project,
                        "context":{
                            "session":{
                                "baseInstructions":parse_stored_json(row.get::<_,Option<String>>(17)?.unwrap_or_default()),
                                "dynamicTools":parse_stored_json(row.get::<_,Option<String>>(18)?.unwrap_or_default()),
                                "selectedCapabilityRoots":parse_stored_json(row.get::<_,Option<String>>(19)?.unwrap_or_default()),
                                "memoryMode":row.get::<_,Option<String>>(20)?,
                                "subagentHistoryStartOrdinal":row.get::<_,Option<i64>>(21)?,
                                "multiAgentVersion":row.get::<_,Option<String>>(22)?,
                                "contextWindow":parse_stored_json(row.get::<_,Option<String>>(23)?.unwrap_or_default())
                            },
                            "runtime":{
                                "cwd":row.get::<_,Option<String>>(4)?,
                                "source":row.get::<_,Option<String>>(5)?,
                                "model":row.get::<_,Option<String>>(6)?
                            }
                        }
                    }))
                },
            )
            .optional()?
            .with_context(|| format!("thread {thread_key} was not found"))?;
        let turns = query_json_connection(
            &connection,
            "SELECT turn_id,status,capture_completeness,completeness_reasons_json,coverage_json,
               started_at_ms,completed_at_ms,execution_context_json,projection_json,provenance_json,last_event_seq
             FROM turns WHERE thread_key=?1 ORDER BY COALESCE(started_at_ms,0),turn_id",
            &[&thread_key],
            |row| {
                Ok(json!({
                    "turnId":row.get::<_,String>(0)?,"status":row.get::<_,String>(1)?,
                    "captureCompleteness":row.get::<_,String>(2)?,
                    "completenessReasons":parse_stored_json(row.get::<_,String>(3)?),
                    "coverage":parse_stored_json(row.get::<_,String>(4)?),
                    "startedAtMs":row.get::<_,Option<i64>>(5)?,"completedAtMs":row.get::<_,Option<i64>>(6)?,
                    "executionContext":row.get::<_,Option<String>>(7)?.map(parse_stored_json),
                    "projection":parse_stored_json(row.get::<_,String>(8)?),
                    "provenance":parse_stored_json(row.get::<_,String>(9)?),"lastEventSeq":row.get::<_,i64>(10)?
                }))
            },
        )?;
        let items = query_json_connection(
            &connection,
            "SELECT turn_scope,item_id,turn_id,item_type,status,started_at_ms,completed_at_ms,summary_text,
               projection_json,provenance_json,last_event_seq FROM items WHERE thread_key=?1
             ORDER BY COALESCE(started_at_ms,0),turn_scope,item_id",
            &[&thread_key],
            |row| {
                Ok(json!({
                    "turnScope":row.get::<_,String>(0)?,"itemId":row.get::<_,String>(1)?,
                    "turnId":row.get::<_,Option<String>>(2)?,"itemType":row.get::<_,String>(3)?,
                    "status":row.get::<_,String>(4)?,"startedAtMs":row.get::<_,Option<i64>>(5)?,
                    "completedAtMs":row.get::<_,Option<i64>>(6)?,"summaryText":row.get::<_,Option<String>>(7)?,
                    "projection":parse_stored_json(row.get::<_,String>(8)?),
                    "provenance":parse_stored_json(row.get::<_,String>(9)?),"lastEventSeq":row.get::<_,i64>(10)?
                }))
            },
        )?;
        let mut events = query_json_connection(
            &connection,
            "SELECT e.event_seq,e.event_id,e.source_id,e.epoch_id,e.source_seq,e.observed_at_ms,e.event_at_ms,
               e.turn_id,e.item_id,e.method,e.phase,e.durability,e.raw_json,e.redaction_json,e.decode_status,
               e.decode_error,e.stored_raw_hash,e.blob_id,b.relative_path
             FROM raw_events e LEFT JOIN blobs b ON b.blob_id=e.blob_id WHERE e.thread_key=?1 ORDER BY e.event_seq",
            &[&thread_key],
            |row| {
                Ok(json!({
                    "eventSeq":row.get::<_,i64>(0)?,"eventId":row.get::<_,String>(1)?,
                    "sourceId":row.get::<_,String>(2)?,"sourceEpoch":row.get::<_,String>(3)?,
                    "sourceSeq":row.get::<_,i64>(4)?,"observedAtMs":row.get::<_,i64>(5)?,
                    "eventAtMs":row.get::<_,Option<i64>>(6)?,"turnId":row.get::<_,Option<String>>(7)?,
                    "itemId":row.get::<_,Option<String>>(8)?,"method":row.get::<_,String>(9)?,
                    "phase":row.get::<_,String>(10)?,"durability":row.get::<_,String>(11)?,
                    "raw":parse_stored_json(row.get::<_,String>(12)?),
                    "redaction":parse_stored_json(row.get::<_,String>(13)?),
                    "decodeStatus":row.get::<_,String>(14)?,"decodeError":row.get::<_,Option<String>>(15)?,
                    "storedRawHash":row.get::<_,String>(16)?,"blobId":row.get::<_,Option<String>>(17)?,
                    "blobRelativePath":row.get::<_,Option<String>>(18)?
                }))
            },
        )?;
        for event in &mut events {
            if let Some(relative_path) = event
                .get("blobRelativePath")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                event["raw"] = parse_stored_json(self.read_blob_relative(&relative_path)?);
            }
            event
                .as_object_mut()
                .map(|event| event.remove("blobRelativePath"));
        }
        connection.execute_batch("COMMIT")?;
        let legacy_redaction_events = events
            .iter()
            .filter(|event| event["redaction"]["ruleVersion"] != "known-secrets-v2")
            .count();
        let export = json!({
            "format":"codex-local-observer-export-v1","exportedAtMs":now_ms(),
            "privacy":{"legacyRedactionEvents":legacy_redaction_events,
              "warning":if legacy_redaction_events > 0 { Some("legacy redaction records were not reprocessed") } else { None }},
            "thread":thread,"turns":turns,"items":items,"rawEvents":events
        });
        write_private_new(output, &serde_json::to_vec_pretty(&export)?)?;
        Ok(ExportReport {
            thread_key: thread_key.to_string(),
            output: output.display().to_string(),
            turns: turns.len(),
            items: items.len(),
            raw_events: events.len(),
            legacy_redaction_events,
        })
    }

    pub fn purge_thread(&self, thread_key: &str) -> Result<PurgeReport> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let (store_source_id, codex_thread_id): (String, String) = transaction
            .query_row(
                "SELECT store_source_id,codex_thread_id FROM threads WHERE thread_key=?1",
                [thread_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .with_context(|| format!("thread {thread_key} was not found"))?;
        let blob_candidates = {
            let mut statement = transaction.prepare(
                "SELECT DISTINCT b.blob_id,b.relative_path FROM blobs b JOIN blob_references r ON r.blob_id=b.blob_id
                 WHERE r.event_seq IN (SELECT event_seq FROM raw_events WHERE thread_key=?1)",
            )?;
            statement
                .query_map([thread_key], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let deleted_turns: usize = transaction.query_row(
            "SELECT COUNT(*) FROM turns WHERE thread_key=?1",
            [thread_key],
            |row| row.get(0),
        )?;
        let deleted_items: usize = transaction.query_row(
            "SELECT COUNT(*) FROM items WHERE thread_key=?1",
            [thread_key],
            |row| row.get(0),
        )?;
        let deleted_raw_events: usize = transaction.query_row(
            "SELECT COUNT(*) FROM raw_events WHERE thread_key=?1",
            [thread_key],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO purged_threads(thread_key,store_source_id,codex_thread_id,purged_at_ms)
             VALUES (?1,?2,?3,?4)",
            params![thread_key, store_source_id, codex_thread_id, now_ms()],
        )?;
        let audit_id = format!("audit_{}", uuid::Uuid::now_v7());
        transaction.execute(
            "INSERT INTO maintenance_audit(audit_id,action,target_kind,target_key,occurred_at_ms,details_json)
             VALUES (?1,'purge','thread',?2,?3,?4)",
            params![
                audit_id,
                thread_key,
                now_ms(),
                json!({"observerCopyOnly":true,"deletedRawEvents":deleted_raw_events,
                  "deletedTurns":deleted_turns,"deletedItems":deleted_items}).to_string()
            ],
        )?;
        transaction.execute(
            "DELETE FROM event_dedupes WHERE original_event_seq IN
             (SELECT event_seq FROM raw_events WHERE thread_key=?1)",
            [thread_key],
        )?;
        transaction.execute(
            "DELETE FROM blob_references WHERE event_seq IN
             (SELECT event_seq FROM raw_events WHERE thread_key=?1)",
            [thread_key],
        )?;
        transaction.execute(
            "DELETE FROM search_index WHERE rowid IN
             (SELECT rowid FROM items WHERE thread_key=?1)",
            [thread_key],
        )?;
        transaction.execute(
            "DELETE FROM pending_requests WHERE thread_key=?1",
            [thread_key],
        )?;
        transaction.execute("DELETE FROM items WHERE thread_key=?1", [thread_key])?;
        transaction.execute("DELETE FROM turns WHERE thread_key=?1", [thread_key])?;
        transaction.execute("DELETE FROM raw_events WHERE thread_key=?1", [thread_key])?;
        transaction.execute(
            "DELETE FROM rollout_locations WHERE store_source_id=?1 AND codex_thread_id=?2",
            params![store_source_id, codex_thread_id],
        )?;
        transaction.execute("DELETE FROM threads WHERE thread_key=?1", [thread_key])?;
        let mut deleted_blob_paths = Vec::new();
        for (blob_id, relative_path) in blob_candidates {
            if transaction.execute(
                "DELETE FROM blobs WHERE blob_id=?1
                 AND NOT EXISTS (SELECT 1 FROM blob_references WHERE blob_id=?1)",
                [&blob_id],
            )? > 0
            {
                deleted_blob_paths.push(relative_path);
            }
        }
        transaction.commit()?;
        for relative_path in &deleted_blob_paths {
            let path = self.resolve_blob_path(relative_path)?;
            if let Err(error) = fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %path.display(), error = %error, "delete purged blob file failed");
            }
        }
        Ok(PurgeReport {
            thread_key: thread_key.to_string(),
            deleted_turns,
            deleted_items,
            deleted_raw_events,
            deleted_blobs: deleted_blob_paths.len(),
            suppression_tombstone: true,
            audit_id,
        })
    }

    pub fn doctor_read_only(config: &Config) -> Result<DoctorReport> {
        let database_path = config.database_path();
        let metadata = fs::symlink_metadata(database_path).ok();
        let direct_file = metadata
            .as_ref()
            .is_some_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
        let mut checks = json!({"permissions":{
            "database":read_only_mode(database_path),
            "token":read_only_mode(&config.server.bearer_token_file),
            "fingerprintKey":read_only_mode(&config.storage.fingerprint_key_file)},
            "schema":null,"wal":null,"quickCheck":null,"checkpoints":0,"retentionLowWatermark":0,
            "unknownEvents":0,"decodeErrors":0,"oversizeErrors":0});
        let (database, mut degraded) = if direct_file {
            match Connection::open_with_flags(database_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
                Ok(connection) => {
                    connection.busy_timeout(std::time::Duration::from_secs(5))?;
                    let integrity = connection
                        .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                        .unwrap_or_else(|error| format!("unreadable: {error}"));
                    let version = connection
                        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                        .unwrap_or(-1);
                    let wal = connection
                        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
                        .unwrap_or_else(|_| "unknown".into());
                    let stats = connection.query_row(
                        "SELECT (SELECT COUNT(*) FROM source_checkpoints),
                          (SELECT COALESCE(value_integer,0) FROM retention_state WHERE key='raw_low_watermark'),
                          (SELECT COUNT(*) FROM raw_events WHERE decode_status='unknown'),
                          (SELECT COUNT(*) FROM raw_events WHERE decode_status='error'),
                          (SELECT COUNT(*) FROM raw_events WHERE decode_error LIKE '%max_raw_event_bytes%')",
                        [], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?,row.get::<_,i64>(4)?)),
                    ).unwrap_or_default();
                    checks["schema"] = json!(version);
                    checks["wal"] = json!(wal);
                    checks["quickCheck"] = json!(integrity);
                    checks["checkpoints"] = json!(stats.0);
                    checks["retentionLowWatermark"] = json!(stats.1);
                    checks["unknownEvents"] = json!(stats.2);
                    checks["decodeErrors"] = json!(stats.3);
                    checks["oversizeErrors"] = json!(stats.4);
                    let healthy =
                        integrity == "ok" && version == LATEST_SCHEMA_VERSION && wal == "wal";
                    (format!("{integrity}; schema_version={version}"), !healthy)
                }
                Err(error) => (format!("unreadable: {error}"), true),
            }
        } else {
            ("missing".into(), true)
        };
        let sources = config
            .sources
            .iter()
            .map(|source| {
                let sessions = source.codex_home.join("sessions");
                let archive = source.codex_home.join("archived_sessions");
                let home_readable = fs::read_dir(&source.codex_home).is_ok();
                let sessions_readable = fs::read_dir(&sessions).is_ok();
                let archive_readable = !archive.exists() || fs::read_dir(&archive).is_ok();
                let readable = home_readable && sessions_readable && archive_readable;
                degraded |= !readable;
                DoctorSource {
                    name: source.name.clone(),
                    path: source.codex_home.display().to_string(),
                    status: if readable {
                        "readable".into()
                    } else if !home_readable {
                        "home_missing_or_unreadable".into()
                    } else if !sessions_readable {
                        "sessions_missing_or_unreadable".into()
                    } else {
                        "archive_unreadable".into()
                    },
                    // The store layer only checks durable rollout access. The command layer
                    // fills this process-runtime check without introducing an upward dependency.
                    session_runtime_status: "retired".into(),
                }
            })
            .collect();
        Ok(DoctorReport {
            status: if degraded {
                "degraded".into()
            } else {
                "healthy".into()
            },
            database,
            sources,
            checks,
        })
    }

    pub fn rebuild_projections(&self) -> Result<usize> {
        let low_watermark = self.retention_low_watermark()?;
        if low_watermark > 0 {
            anyhow::bail!(
                "projection rebuild is unsafe after raw retention (low watermark {low_watermark})"
            );
        }
        let policy_omissions: i64 = self.read_connection()?.query_row(
            "SELECT COUNT(*) FROM raw_events WHERE json_extract(raw_json,'$.policy')='omitted'",
            [],
            |row| row.get(0),
        )?;
        if policy_omissions > 0 {
            anyhow::bail!(
                "projection rebuild is unavailable because {policy_omissions} raw events were intentionally omitted by capture policy"
            );
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "DELETE FROM blob_references WHERE reference_kind='projection';
             DELETE FROM projection_conflicts; DELETE FROM pending_requests;
             DELETE FROM search_index; DELETE FROM items; DELETE FROM turns; DELETE FROM threads;",
        )?;
        let events = {
            let mut statement = transaction.prepare(
                "SELECT event_seq,event_id,source_id,epoch_id,source_seq,dedupe_key,observed_at_ms,event_at_ms,
                 thread_key,codex_thread_id,turn_id,item_id,method,phase,durability,source_fingerprint,stored_raw_hash,
                 raw_json,redaction_json,decode_status,decode_error,request_id,store_source_id,blob_id,
                 (SELECT relative_path FROM blobs WHERE blobs.blob_id=raw_events.blob_id)
                 FROM raw_events ORDER BY event_seq"
            )?;
            let rows = statement.query_map([], |row| {
                let raw_json: String = row.get(17)?;
                let stored_raw_hash: String = row.get(16)?;
                let blob_id: Option<String> = row.get(23)?;
                let relative_path: Option<String> = row.get(24)?;
                let materialized_raw = if let Some(relative_path) = relative_path {
                    self.read_blob_relative(&relative_path).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            24,
                            rusqlite::types::Type::Text,
                            error.into(),
                        )
                    })?
                } else {
                    raw_json.clone()
                };
                if blake3::hash(materialized_raw.as_bytes()).to_hex().as_str() != stored_raw_hash {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        24,
                        rusqlite::types::Type::Text,
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "blob content hash does not match raw event",
                        )
                        .into(),
                    ));
                }
                let raw: Value = serde_json::from_str(&materialized_raw).unwrap_or(Value::Null);
                let durability: String = row.get(14)?;
                let method: String = row.get(12)?;
                let phase: String = row.get(13)?;
                let (top_type, payload, item_type, event_summary) = if durability == "transient" {
                    let params = raw
                        .get("params")
                        .or_else(|| raw.get("result"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    let item = params.get("item").cloned();
                    let payload = item
                        .clone()
                        .or_else(|| params.get("turn").cloned())
                        .or_else(|| params.get("thread").cloned())
                        .unwrap_or_else(|| params.clone());
                    (
                        "app_server".to_string(),
                        payload,
                        classify_live_item(&method, item.as_ref()),
                        live_summary(&params, item.as_ref()),
                    )
                } else {
                    (
                        raw.get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        raw.get("payload").cloned().unwrap_or(Value::Null),
                        classify_item(&raw),
                        summary_text(&raw),
                    )
                };
                let payload = if blob_id.is_some() && item_type.is_some() {
                    serde_json::from_str::<Value>(&raw_json)
                        .ok()
                        .and_then(|stub| stub.get("blobRef").cloned())
                        .map(|blob_ref| json!({"blobRefs":[blob_ref]}))
                        .unwrap_or(payload)
                } else {
                    payload
                };
                Ok((
                    row.get::<_, i64>(0)?,
                    NormalizedEvent {
                        event_id: row.get(1)?,
                        source_id: row.get(2)?,
                        store_source_id: row.get(22)?,
                        epoch_id: row.get(3)?,
                        source_seq: row.get(4)?,
                        dedupe_key: row.get(5)?,
                        observed_at_ms: row.get(6)?,
                        event_at_ms: row.get(7)?,
                        thread_key: row.get(8)?,
                        codex_thread_id: row.get(9)?,
                        turn_id: row.get(10)?,
                        item_id: row.get(11)?,
                        request_id: row.get(21)?,
                        blob_id,
                        protocol_direction: None,
                        worker_id: None,
                        worker_connection_epoch: None,
                        proxy_seq: None,
                        method,
                        phase: phase.clone(),
                        durability,
                        projectable: !row.get::<_, String>(8)?.is_empty(),
                        source_fingerprint: row.get(15)?,
                        stored_raw_hash,
                        raw_json,
                        redaction_json: row.get(18)?,
                        decode_status: row.get(19)?,
                        decode_error: row.get(20)?,
                        top_type,
                        item_type,
                        item_status: Some(
                            match phase.as_str() {
                                "started" | "request" => "started",
                                "delta" => "streaming",
                                _ => "completed",
                            }
                            .into(),
                        ),
                        summary_text: event_summary,
                        payload,
                    },
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (seq, event) in &events {
            project_event(&transaction, event, *seq)?;
            update_event_coverage(&transaction, event)?;
            let reference_key = projection_reference_key(event);
            if let Some(blob_id) = event
                .blob_id
                .as_deref()
                .filter(|_| event.item_type.is_some())
            {
                transaction.execute(
                    "INSERT INTO blob_references(reference_kind,reference_key,blob_id,event_seq)
                     VALUES ('projection',?1,?2,?3)
                     ON CONFLICT(reference_kind,reference_key) DO UPDATE SET
                       blob_id=excluded.blob_id,event_seq=excluded.event_seq",
                    params![reference_key, blob_id, seq],
                )?;
            } else {
                transaction.execute(
                    "DELETE FROM blob_references WHERE reference_kind='projection' AND reference_key=?1",
                    [reference_key],
                )?;
            }
        }
        recompute_all_completeness(&transaction)?;
        transaction.commit()?;
        Ok(events.len())
    }

    pub fn query_json(
        &self,
        sql: &str,
        parameters: &[&dyn rusqlite::ToSql],
        mapper: fn(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
    ) -> Result<Vec<Value>> {
        let connection = self.read_connection()?;
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
        let connection = self.read_connection()?;
        let mut statement = connection.prepare(sql)?;
        Ok(statement
            .query_map(rusqlite::params_from_iter(parameters), mapper)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn load_coverage(
    connection: &Connection,
    thread_key: &str,
    turn_id: &str,
) -> Result<CoverageFlags> {
    let stored = connection
        .query_row(
            "SELECT coverage_json FROM turns WHERE thread_key=?1 AND turn_id=?2",
            params![thread_key, turn_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(stored
        .as_deref()
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default())
}

fn coverage_state(coverage: &CoverageFlags) -> (&'static str, Vec<&'static str>) {
    let durable_any =
        coverage.durable_started || coverage.durable_terminal || coverage.durable_eof_reached;
    let live_any = coverage.live_started
        || coverage.live_terminal
        || coverage.live_epoch_id.is_some()
        || coverage.source_disconnect_count > 0;
    if coverage.coverage_evidence_retained_incomplete {
        return (
            if durable_any {
                "durable_partial"
            } else if live_any {
                "live_partial"
            } else {
                "metadata_only"
            },
            vec!["coverage_evidence_retained_incomplete"],
        );
    }
    if durable_any {
        if coverage.durable_started && coverage.durable_terminal && coverage.durable_eof_reached {
            return ("durable_complete", Vec::new());
        }
        let mut reasons = Vec::new();
        if !coverage.durable_started {
            reasons.push("durable_turn_start_missing");
        }
        if !coverage.durable_terminal {
            reasons.push("durable_turn_not_terminal");
        }
        if !coverage.durable_eof_reached {
            reasons.push("durable_eof_not_reached");
        }
        return ("durable_partial", reasons);
    }
    if live_any {
        if coverage.live_started
            && coverage.live_terminal
            && coverage.live_epoch_contiguous
            && coverage.source_disconnect_count == 0
            && coverage.decode_error_count == 0
        {
            return ("live_complete", Vec::new());
        }
        let mut reasons = Vec::new();
        if !coverage.live_started {
            reasons.push("attached_after_turn_started");
        }
        if !coverage.live_terminal {
            reasons.push("live_turn_not_terminal");
        }
        if !coverage.live_epoch_contiguous {
            reasons.push("live_epoch_incomplete");
        }
        if coverage.source_disconnect_count > 0 {
            reasons.push("source_disconnected");
        }
        if coverage.decode_error_count > 0 {
            reasons.push("live_decode_error");
        }
        return ("live_partial", reasons);
    }
    ("metadata_only", Vec::new())
}

fn store_coverage(
    connection: &Connection,
    thread_key: &str,
    turn_id: &str,
    coverage: &CoverageFlags,
) -> Result<()> {
    let (state, reasons) = coverage_state(coverage);
    connection.execute(
        "UPDATE turns SET coverage_json=?1,capture_completeness=?2,completeness_reasons_json=?3
         WHERE thread_key=?4 AND turn_id=?5",
        params![
            serde_json::to_string(coverage)?,
            state,
            serde_json::to_string(&reasons)?,
            thread_key,
            turn_id
        ],
    )?;
    Ok(())
}

fn update_event_coverage(connection: &Connection, event: &NormalizedEvent) -> Result<()> {
    let Some(turn_id) = event.turn_id.as_deref() else {
        return Ok(());
    };
    let exists = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM turns WHERE thread_key=?1 AND turn_id=?2)",
        params![event.thread_key, turn_id],
        |row| row.get::<_, bool>(0),
    )?;
    if !exists {
        return Ok(());
    }
    let mut coverage = load_coverage(connection, &event.thread_key, turn_id)?;
    if event.decode_status == "error" {
        coverage.decode_error_count = coverage.decode_error_count.saturating_add(1);
        if event.durability == "transient" {
            coverage.live_epoch_contiguous = false;
        }
    } else if event.decode_status == "unknown" {
        coverage.unknown_event_count = coverage.unknown_event_count.saturating_add(1);
    }
    store_coverage(connection, &event.thread_key, turn_id, &coverage)
}

fn recompute_thread_completeness(connection: &Connection, thread_key: &str) -> Result<()> {
    let turns = {
        let mut statement = connection.prepare(
            "SELECT capture_completeness,completeness_reasons_json,coverage_json
             FROM turns WHERE thread_key=?1",
        )?;
        statement
            .query_map([thread_key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let (state, reasons) = if turns.is_empty() {
        ("metadata_only", Vec::<String>::new())
    } else {
        let mut reasons = std::collections::BTreeSet::new();
        let mut has_ephemeral_lost = false;
        let mut has_partial = false;
        let mut has_durable_evidence = false;
        let mut all_durable_complete = true;
        let mut has_live_only_complete = false;
        for (turn_state, turn_reasons, coverage_json) in &turns {
            let coverage: CoverageFlags = serde_json::from_str(coverage_json).unwrap_or_default();
            has_durable_evidence |= coverage.durable_started
                || coverage.durable_terminal
                || coverage.durable_eof_reached;
            has_ephemeral_lost |= turn_state == "ephemeral_lost";
            has_partial |= turn_state.ends_with("_partial");
            all_durable_complete &= turn_state == "durable_complete";
            has_live_only_complete |= turn_state == "live_complete";
            if let Ok(values) = serde_json::from_str::<Vec<String>>(turn_reasons) {
                reasons.extend(values);
            }
        }
        let state = if has_ephemeral_lost {
            "ephemeral_lost"
        } else if has_partial {
            if has_durable_evidence {
                "durable_partial"
            } else {
                "live_partial"
            }
        } else if all_durable_complete {
            "durable_complete"
        } else if has_live_only_complete {
            reasons.insert("contains_live_only_turns".into());
            "live_complete"
        } else {
            "metadata_only"
        };
        (state, reasons.into_iter().collect())
    };
    connection.execute(
        "UPDATE threads SET capture_completeness=?1,completeness_reasons_json=?2 WHERE thread_key=?3",
        params![state, serde_json::to_string(&reasons)?, thread_key],
    )?;
    Ok(())
}

fn recompute_all_completeness(connection: &Connection) -> Result<()> {
    let retained_incomplete = connection
        .query_row(
            "SELECT COALESCE(value_integer,0) FROM retention_state WHERE key='raw_low_watermark'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0)
        > 0;
    let turn_keys = {
        let mut statement = connection.prepare("SELECT thread_key,turn_id FROM turns")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (thread_key, turn_id) in &turn_keys {
        let mut coverage = load_coverage(connection, thread_key, turn_id)?;
        let (decode_errors, unknown_events, durable_started, durable_terminal):
            (Option<i64>, Option<i64>, Option<i64>, Option<i64>) = connection.query_row(
            "SELECT
               SUM(CASE WHEN decode_status='error' THEN 1 ELSE 0 END),
               SUM(CASE WHEN decode_status='unknown' THEN 1 ELSE 0 END),
               MAX(CASE WHEN durability='durable' AND method IN ('event/task_started','event/turn_started') THEN 1 ELSE 0 END),
               MAX(CASE WHEN durability='durable' AND method IN ('event/task_complete','event/turn_complete','event/turn_aborted') THEN 1 ELSE 0 END)
             FROM raw_events WHERE thread_key=?1 AND turn_id=?2",
            params![thread_key, turn_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        coverage.decode_error_count = coverage
            .decode_error_count
            .max(decode_errors.unwrap_or(0).max(0) as u64);
        coverage.unknown_event_count = coverage
            .unknown_event_count
            .max(unknown_events.unwrap_or(0).max(0) as u64);
        coverage.durable_started |= durable_started.unwrap_or(0) > 0;
        coverage.durable_terminal |= durable_terminal.unwrap_or(0) > 0;
        coverage.durable_eof_reached |= coverage.durable_terminal;
        coverage.coverage_evidence_retained_incomplete |= retained_incomplete;
        store_coverage(connection, thread_key, turn_id, &coverage)?;
    }
    let thread_keys = {
        let mut statement = connection.prepare("SELECT thread_key FROM threads")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for thread_key in thread_keys {
        recompute_thread_completeness(connection, &thread_key)?;
    }
    Ok(())
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
        params![event.thread_key, event.store_source_id, event.codex_thread_id, time,
                json!({"lastEventSeq":event_seq,"sourceId":event.source_id}).to_string(), event_seq],
    )?;

    if event.durability == "transient" {
        project_live_lifecycle(transaction, event, event_seq, time)?;
    }

    match event.top_type.as_str() {
        "session_meta" => {
            let p = &event.payload;
            record_thread_conflicts(transaction, event, event_seq)?;
            let parent_thread_id = p
                .get("parent_thread_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let forked_from_id = p
                .get("forked_from_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let parent_thread_key = parent_thread_id
                .as_deref()
                .map(|id| thread_key(&event.store_source_id, id));
            let forked_from_thread_key = forked_from_id
                .as_deref()
                .map(|id| thread_key(&event.store_source_id, id));
            let history_base = p.get("history_base").filter(|value| !value.is_null());
            let project_key_value = p.get("cwd").and_then(Value::as_str).and_then(|cwd| {
                inferred_project_key(cwd, p.get("originator").and_then(Value::as_str))
            });
            let base_instructions = p
                .get("base_instructions")
                .filter(|value| !value.is_null())
                .map(Value::to_string);
            let dynamic_tools = p
                .get("dynamic_tools")
                .filter(|value| !value.is_null())
                .map(Value::to_string);
            let selected_capability_roots = p
                .get("selected_capability_roots")
                .filter(|value| !value.is_null())
                .map(Value::to_string);
            let memory_mode = p
                .get("memory_mode")
                .and_then(Value::as_str)
                .map(str::to_string);
            let subagent_history_start_ordinal = p
                .get("subagent_history_start_ordinal")
                .and_then(Value::as_i64);
            let multi_agent_version = p
                .get("multi_agent_version")
                .filter(|value| !value.is_null())
                .map(Value::to_string);
            let context_window = p
                .get("context_window")
                .filter(|value| !value.is_null())
                .map(Value::to_string);
            let session_projection = json!({
                "sessionId":p.get("session_id").or_else(|| p.get("id")),
                "parentThreadId":parent_thread_id,"forkedFromId":forked_from_id,
                "cwd":p.get("cwd"),"source":p.get("source"),"threadSource":p.get("thread_source"),
                "originator":p.get("originator"),"cliVersion":p.get("cli_version"),
                "agentNickname":p.get("agent_nickname"),"agentRole":p.get("agent_role"),"agentPath":p.get("agent_path"),
                "modelProvider":p.get("model_provider"),"historyMode":p.get("history_mode"),"historyBase":history_base,
                "baseInstructions":p.get("base_instructions"),"dynamicTools":p.get("dynamic_tools"),
                "selectedCapabilityRoots":p.get("selected_capability_roots"),"memoryMode":p.get("memory_mode"),
                "subagentHistoryStartOrdinal":p.get("subagent_history_start_ordinal"),
                "multiAgentVersion":p.get("multi_agent_version"),"contextWindow":p.get("context_window")
            });
            transaction.execute(
                "UPDATE threads SET session_id=COALESCE(?1,session_id),cwd=COALESCE(?2,cwd),source=COALESCE(?3,source),
                   model_provider=COALESCE(?4,model_provider),created_at_ms=COALESCE(?5,created_at_ms),
                   parent_thread_id=?6,parent_thread_key=?7,forked_from_id=?8,forked_from_thread_key=?9,
                   agent_nickname=?10,agent_role=?11,agent_path=?12,originator=?13,cli_version=?14,
                   thread_source=?15,history_mode=?16,history_base_json=?17,
                   project_key=CASE WHEN ?2 IS NOT NULL THEN ?18 ELSE project_key END,
                   base_instructions_json=COALESCE(?19,base_instructions_json),
                   dynamic_tools_json=COALESCE(?20,dynamic_tools_json),
                   selected_capability_roots_json=COALESCE(?21,selected_capability_roots_json),
                   memory_mode=COALESCE(?22,memory_mode),
                   subagent_history_start_ordinal=COALESCE(?23,subagent_history_start_ordinal),
                   multi_agent_version=COALESCE(?24,multi_agent_version),
                   context_window_json=COALESCE(?25,context_window_json),
                   projection_json=?26,provenance_json=?27,last_event_seq=?28 WHERE thread_key=?29",
                params![
                    p.get("session_id").or_else(|| p.get("id")).and_then(Value::as_str),
                    p.get("cwd").and_then(Value::as_str),value_as_string(p.get("source")),
                    p.get("model_provider").and_then(Value::as_str),
                    p.get("timestamp").and_then(Value::as_str).and_then(parse_time_ms),
                    parent_thread_id,parent_thread_key,forked_from_id,forked_from_thread_key,
                    p.get("agent_nickname").and_then(Value::as_str),p.get("agent_role").and_then(Value::as_str),
                    p.get("agent_path").and_then(Value::as_str),p.get("originator").and_then(Value::as_str),
                    p.get("cli_version").and_then(Value::as_str),value_as_string(p.get("thread_source")),
                    value_as_string(p.get("history_mode")),history_base.map(Value::to_string),
                    project_key_value,base_instructions,dynamic_tools,selected_capability_roots,
                    memory_mode,subagent_history_start_ordinal,multi_agent_version,context_window,
                    session_projection.to_string(),
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
                "UPDATE threads SET cwd=COALESCE(?1,cwd),model=COALESCE(?2,model),reasoning_effort=?3,
                   approval_policy=?4,approvals_reviewer_json=?5,sandbox_json=?6,active_permission_profile_json=?7,
                   projection_json=?8 WHERE thread_key=?9",
                params![event.payload.get("cwd").and_then(Value::as_str), event.payload.get("model").and_then(Value::as_str),
                    value_as_string(event.payload.get("effort")),value_as_string(event.payload.get("approval_policy")),
                    event.payload.get("approvals_reviewer").filter(|value| !value.is_null()).map(Value::to_string),
                    event.payload.get("sandbox_policy").filter(|value| !value.is_null()).map(Value::to_string),
                    event.payload.get("permission_profile").filter(|value| !value.is_null()).map(Value::to_string),
                    projection,event.thread_key],
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
                    "UPDATE threads SET runtime_status='idle' WHERE thread_key=?1",
                    [&event.thread_key],
                )?;
            } else if nested_type == "turn_aborted"
                && let Some(turn_id) = event.turn_id.as_deref()
            {
                upsert_turn(
                    transaction,
                    event,
                    event_seq,
                    TurnUpdate {
                        turn_id,
                        status: "interrupted",
                        started: event
                            .payload
                            .get("started_at")
                            .and_then(Value::as_i64)
                            .map(|v| v * 1000)
                            .unwrap_or(time),
                        completed: Some(time),
                        durable_started: true,
                    },
                )?;
                transaction.execute(
                    "UPDATE threads SET runtime_status='idle' WHERE thread_key=?1",
                    [&event.thread_key],
                )?;
            }
        }
        _ => {}
    }

    if event.phase == "request"
        && event.protocol_direction.as_deref() != Some("tui_to_upstream")
        && let Some(request_id) = event.request_id.as_deref()
    {
        let method = event.method.to_ascii_lowercase();
        let request_type = if method.contains("approval") {
            "approval"
        } else if method.contains("userinput") || method.contains("user_input") {
            "user_input"
        } else if method.contains("elicitation") {
            "mcp_elicitation"
        } else {
            "unknown"
        };
        transaction.execute(
            "INSERT INTO pending_requests(source_id,epoch_id,request_id,thread_key,request_type,state,
               request_event_seq,payload_json) VALUES (?1,?2,?3,?4,?5,'pending',?6,?7)
             ON CONFLICT(source_id,epoch_id,request_id) DO UPDATE SET
               thread_key=excluded.thread_key,request_type=excluded.request_type,state='pending',
               request_event_seq=excluded.request_event_seq,resolved_event_seq=NULL,
               payload_json=excluded.payload_json,request_version=pending_requests.request_version+1,
               resolving_command_id=NULL,resolving_started_at_ms=NULL
             WHERE excluded.request_event_seq>pending_requests.request_event_seq",
            params![event.source_id,event.epoch_id,request_id,event.thread_key,request_type,event_seq,event.payload.to_string()],
        )?;
    }
    if event.phase == "resolved"
        && let Some(request_id) = event.request_id.as_deref()
    {
        transaction.execute(
            "UPDATE pending_requests SET state='resolved',resolved_event_seq=?1
             WHERE source_id=?2 AND epoch_id=?3 AND request_id=?4
               AND state IN ('pending','resolving')",
            params![event_seq, event.source_id, event.epoch_id, request_id],
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
        let append_delta = event.durability == "transient" && event.phase == "delta";
        let provenance_source = if event.durability == "transient" {
            "app_server"
        } else {
            "rollout"
        };
        if event.durability == "durable" {
            record_item_conflicts(
                transaction,
                event,
                event_seq,
                &turn_scope,
                item_id,
                status,
                event.summary_text.as_deref(),
                &projection,
            )?;
        }
        transaction.execute(
            "INSERT INTO items(thread_key,turn_scope,item_id,turn_id,item_type,status,started_at_ms,completed_at_ms,
               summary_text,projection_json,provenance_json,last_event_seq)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(thread_key,turn_scope,item_id) DO UPDATE SET
               status=CASE WHEN items.status IN ('completed','failed') AND excluded.status NOT IN ('completed','failed')
                 THEN items.status ELSE excluded.status END,
               completed_at_ms=COALESCE(excluded.completed_at_ms,items.completed_at_ms),
               summary_text=CASE WHEN ?13 THEN COALESCE(items.summary_text,'') || COALESCE(excluded.summary_text,'')
                 ELSE COALESCE(excluded.summary_text,items.summary_text) END,projection_json=excluded.projection_json,
               provenance_json=excluded.provenance_json,last_event_seq=excluded.last_event_seq",
            params![event.thread_key, turn_scope, item_id, event.turn_id, item_type, status, time,
                    if status == "completed" || status == "failed" { Some(time) } else { None }, event.summary_text,
                    projection, json!({"eventSeq":event_seq,"source":provenance_source,"epoch":event.epoch_id}).to_string(), event_seq,
                    append_delta],
        )?;
        let (item_rowid, indexed_summary) = transaction.query_row(
            "SELECT rowid,summary_text FROM items
             WHERE thread_key=?1 AND turn_scope=?2 AND item_id=?3",
            params![event.thread_key, turn_scope, item_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
        )?;
        transaction.execute("DELETE FROM search_index WHERE rowid=?1", [item_rowid])?;
        let indexed_summary = indexed_summary.filter(|text| !text.trim().is_empty());
        if let Some(summary) = indexed_summary.as_deref() {
            let entity_key = format!("{}:{}:{}", event.thread_key, turn_scope, item_id);
            transaction.execute(
                "INSERT INTO search_index(rowid,entity_key,thread_key,item_id,text)
                 VALUES (?1,?2,?3,?4,?5)",
                params![item_rowid, entity_key, event.thread_key, item_id, summary],
            )?;
            transaction.execute(
                "UPDATE threads SET last_message_preview=?1 WHERE thread_key=?2",
                params![truncate(summary, 240), event.thread_key],
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn record_item_conflicts(
    transaction: &Transaction<'_>,
    event: &NormalizedEvent,
    durable_event_seq: i64,
    turn_scope: &str,
    item_id: &str,
    durable_status: &str,
    durable_summary: Option<&str>,
    durable_projection: &str,
) -> Result<()> {
    let existing = transaction
        .query_row(
            "SELECT status,summary_text,projection_json,provenance_json,last_event_seq
             FROM items WHERE thread_key=?1 AND turn_scope=?2 AND item_id=?3",
            params![event.thread_key, turn_scope, item_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((live_status, live_summary, live_projection, provenance, live_event_seq)) = existing
    else {
        return Ok(());
    };
    if serde_json::from_str::<Value>(&provenance)
        .ok()
        .and_then(|value| {
            value
                .get("source")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .as_deref()
        != Some("app_server")
    {
        return Ok(());
    }
    let entity_key = format!("{turn_scope}:{item_id}");
    for (field, live, durable) in [
        ("status", json!(live_status), json!(durable_status)),
        ("summary_text", json!(live_summary), json!(durable_summary)),
        (
            "projection_json",
            parse_stored_json(live_projection),
            parse_stored_json(durable_projection.to_string()),
        ),
    ] {
        if live == durable {
            transaction.execute(
                "UPDATE projection_conflicts SET status='resolved',resolved_at_ms=?1
                 WHERE thread_key=?2 AND entity_type='item' AND entity_key=?3 AND field_name=?4 AND status='active'",
                params![now_ms(),event.thread_key,entity_key,field],
            )?;
            continue;
        }
        let conflict_id = blake3::hash(
            format!(
                "{}\0item\0{}\0{}\0{}\0{}",
                event.thread_key, entity_key, field, live_event_seq, durable_event_seq
            )
            .as_bytes(),
        )
        .to_hex()
        .to_string();
        transaction.execute(
            "INSERT INTO projection_conflicts(conflict_id,thread_key,entity_type,entity_key,field_name,
               live_event_seq,durable_event_seq,live_value_json,durable_value_json,status,detected_at_ms)
             VALUES (?1,?2,'item',?3,?4,?5,?6,?7,?8,'active',?9)
             ON CONFLICT(conflict_id) DO UPDATE SET status='active',resolved_at_ms=NULL",
            params![conflict_id,event.thread_key,entity_key,field,live_event_seq,durable_event_seq,
                live.to_string(),durable.to_string(),now_ms()],
        )?;
    }
    Ok(())
}

fn record_thread_conflicts(
    transaction: &Transaction<'_>,
    event: &NormalizedEvent,
    durable_event_seq: i64,
) -> Result<()> {
    let existing = transaction.query_row(
        "SELECT cwd,source,model_provider,provenance_json,last_event_seq FROM threads WHERE thread_key=?1",
        [&event.thread_key],
        |row| Ok((row.get::<_,Option<String>>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,Option<String>>(2)?,
            row.get::<_,String>(3)?,row.get::<_,i64>(4)?)),
    ).optional()?;
    let Some((live_cwd, live_source, live_model_provider, provenance, live_event_seq)) = existing
    else {
        return Ok(());
    };
    if serde_json::from_str::<Value>(&provenance)
        .ok()
        .and_then(|value| {
            value
                .get("source")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .as_deref()
        != Some("app_server")
    {
        return Ok(());
    }
    let durable = [
        (
            "cwd",
            event
                .payload
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::to_string),
            live_cwd,
        ),
        (
            "source",
            value_as_string(event.payload.get("source")),
            live_source,
        ),
        (
            "model_provider",
            event
                .payload
                .get("model_provider")
                .and_then(Value::as_str)
                .map(str::to_string),
            live_model_provider,
        ),
    ];
    for (field, durable_value, live_value) in durable {
        let (Some(durable_value), Some(live_value)) = (durable_value, live_value) else {
            continue;
        };
        if durable_value == live_value {
            continue;
        }
        let conflict_id = blake3::hash(
            format!(
                "{}\0thread\0{}\0{}\0{}",
                event.thread_key, field, live_event_seq, durable_event_seq
            )
            .as_bytes(),
        )
        .to_hex()
        .to_string();
        transaction.execute(
            "INSERT INTO projection_conflicts(conflict_id,thread_key,entity_type,entity_key,field_name,
               live_event_seq,durable_event_seq,live_value_json,durable_value_json,status,detected_at_ms)
             VALUES (?1,?2,'thread',?2,?3,?4,?5,?6,?7,'active',?8)
             ON CONFLICT(conflict_id) DO UPDATE SET status='active',resolved_at_ms=NULL",
            params![conflict_id,event.thread_key,field,live_event_seq,durable_event_seq,json!(live_value).to_string(),json!(durable_value).to_string(),now_ms()],
        )?;
    }
    Ok(())
}

fn project_live_lifecycle(
    tx: &Transaction<'_>,
    event: &NormalizedEvent,
    event_seq: i64,
    time: i64,
) -> Result<()> {
    match event.method.as_str() {
        "thread/started" => {
            tx.execute(
                "UPDATE threads SET runtime_status=COALESCE(?1,runtime_status),runtime_status_stale=0,
                   projection_json=?2,last_event_seq=?3 WHERE thread_key=?4",
                params![live_status(event.payload.get("status")),event.payload.to_string(),event_seq,event.thread_key],
            )?;
        }
        "thread/status/changed" => {
            tx.execute(
                "UPDATE threads SET runtime_status=?1,runtime_status_stale=0,last_event_seq=?2 WHERE thread_key=?3",
                params![live_status(event.payload.get("status")),event_seq,event.thread_key],
            )?;
        }
        "thread/read/response" => {
            tx.execute(
                "UPDATE threads SET name=COALESCE(?1,name),cwd=COALESCE(?2,cwd),source=COALESCE(?3,source),
                   model=COALESCE(?4,model),archived=COALESCE(?5,archived),runtime_status=COALESCE(?6,runtime_status),
                   runtime_status_stale=0,projection_json=?7,provenance_json=?8,last_event_seq=?9 WHERE thread_key=?10",
                params![event.payload.get("name").or_else(|| event.payload.get("title")).and_then(Value::as_str),
                    event.payload.get("cwd").and_then(Value::as_str),value_as_string(event.payload.get("source")),
                    event.payload.get("model").and_then(Value::as_str),event.payload.get("archived").and_then(Value::as_bool),
                    live_status(event.payload.get("status")),event.payload.to_string(),
                    json!({"eventSeq":event_seq,"source":"app_server","epoch":event.epoch_id,"method":"thread/read"}).to_string(),
                    event_seq,event.thread_key],
            )?;
        }
        "thread/settings/updated" => {
            let settings = event
                .payload
                .get("threadSettings")
                .unwrap_or(&event.payload);
            tx.execute(
                "UPDATE threads SET cwd=COALESCE(?1,cwd),model=COALESCE(?2,model),
                   reasoning_effort=COALESCE(?3,reasoning_effort),
                   approval_policy=COALESCE(?4,approval_policy),
                   approvals_reviewer_json=COALESCE(?5,approvals_reviewer_json),
                   sandbox_json=COALESCE(?6,sandbox_json),
                   active_permission_profile_json=COALESCE(?7,active_permission_profile_json),
                   runtime_status_stale=0,projection_json=?8,last_event_seq=?9 WHERE thread_key=?10",
                params![
                    settings.get("cwd").and_then(Value::as_str),
                    settings.get("model").and_then(Value::as_str),
                    value_as_string(settings.get("effort")),
                    value_as_string(settings.get("approvalPolicy")),
                    settings.get("approvalsReviewer").map(Value::to_string),
                    settings.get("sandboxPolicy").map(Value::to_string),
                    settings.get("activePermissionProfile").map(Value::to_string),
                    event.payload.to_string(),
                    event_seq,
                    event.thread_key,
                ],
            )?;
        }
        "thread/goal/updated" | "thread/goal/get/response" | "thread/goal/set/response" => {
            let goal = event.payload.get("goal");
            if let Some(goal) = goal.filter(|value| !value.is_null()) {
                let status = goal
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("active");
                tx.execute(
                    "INSERT INTO thread_goals(thread_key,source_id,source_epoch,status,goal_json,
                       updated_event_seq,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7)
                     ON CONFLICT(thread_key) DO UPDATE SET source_id=excluded.source_id,
                       source_epoch=excluded.source_epoch,status=excluded.status,
                       goal_json=excluded.goal_json,updated_event_seq=excluded.updated_event_seq,
                       updated_at_ms=excluded.updated_at_ms",
                    params![
                        event.thread_key,
                        event.source_id,
                        event.epoch_id,
                        status,
                        goal.to_string(),
                        event_seq,
                        time,
                    ],
                )?;
            } else if event.method == "thread/goal/get/response" {
                tx.execute(
                    "DELETE FROM thread_goals WHERE thread_key=?1",
                    [&event.thread_key],
                )?;
            }
        }
        "thread/goal/cleared" | "thread/goal/clear/response" => {
            tx.execute(
                "DELETE FROM thread_goals WHERE thread_key=?1",
                [&event.thread_key],
            )?;
        }
        "turn/started" => {
            if let Some(turn_id) = event.turn_id.as_deref() {
                upsert_live_turn(
                    tx, event, event_seq, turn_id, "running", time, None, true, false,
                )?;
                tx.execute(
                    "UPDATE threads SET runtime_status='active',runtime_status_stale=0
                     WHERE thread_key=?1",
                    [&event.thread_key],
                )?;
            }
        }
        "turn/completed" => {
            if let Some(turn_id) = event.turn_id.as_deref() {
                let live_started = tx
                    .query_row(
                        "SELECT coverage_json FROM turns WHERE thread_key=?1 AND turn_id=?2",
                        params![event.thread_key, turn_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .and_then(|coverage| serde_json::from_str::<Value>(&coverage).ok())
                    .and_then(|coverage| coverage.get("liveStarted").and_then(Value::as_bool))
                    .unwrap_or(false);
                let status = event
                    .payload
                    .get("status")
                    .and_then(|status| status.get("type").unwrap_or(status).as_str())
                    .unwrap_or("completed");
                upsert_live_turn(
                    tx,
                    event,
                    event_seq,
                    turn_id,
                    status,
                    time,
                    Some(time),
                    live_started,
                    true,
                )?;
                tx.execute(
                    "UPDATE threads SET runtime_status='idle',runtime_status_stale=0 WHERE thread_key=?1",
                    [&event.thread_key],
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn upsert_live_turn(
    tx: &Transaction<'_>,
    event: &NormalizedEvent,
    seq: i64,
    turn_id: &str,
    status: &str,
    started: i64,
    completed: Option<i64>,
    live_started: bool,
    live_terminal: bool,
) -> Result<()> {
    let mut coverage = load_coverage(tx, &event.thread_key, turn_id)?;
    if live_started {
        if coverage.live_epoch_id.as_deref() != Some(event.epoch_id.as_str()) {
            coverage.live_epoch_id = Some(event.epoch_id.clone());
            coverage.live_started = true;
            coverage.live_terminal = false;
            coverage.live_epoch_contiguous = true;
        } else {
            coverage.live_started = true;
        }
    }
    if live_terminal {
        coverage.live_terminal = true;
        coverage.live_epoch_contiguous &= coverage.live_started
            && coverage.live_epoch_id.as_deref() == Some(event.epoch_id.as_str());
    }
    let (completeness, reasons) = coverage_state(&coverage);
    tx.execute(
        "INSERT INTO turns(thread_key,turn_id,status,capture_completeness,completeness_reasons_json,coverage_json,
           started_at_ms,completed_at_ms,projection_json,provenance_json,last_event_seq)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(thread_key,turn_id) DO UPDATE SET
           status=CASE WHEN turns.capture_completeness='durable_complete' THEN turns.status ELSE excluded.status END,
           capture_completeness=CASE WHEN turns.capture_completeness='durable_complete' THEN turns.capture_completeness ELSE excluded.capture_completeness END,
           completeness_reasons_json=CASE WHEN turns.capture_completeness='durable_complete' THEN turns.completeness_reasons_json ELSE excluded.completeness_reasons_json END,
           coverage_json=CASE WHEN turns.capture_completeness='durable_complete' THEN turns.coverage_json ELSE excluded.coverage_json END,
           completed_at_ms=COALESCE(excluded.completed_at_ms,turns.completed_at_ms),
           projection_json=CASE WHEN turns.capture_completeness='durable_complete' THEN turns.projection_json ELSE excluded.projection_json END,
           provenance_json=CASE WHEN turns.capture_completeness='durable_complete' THEN turns.provenance_json ELSE excluded.provenance_json END,
           last_event_seq=excluded.last_event_seq",
        params![event.thread_key,turn_id,status,completeness,
            serde_json::to_string(&reasons)?,serde_json::to_string(&coverage)?,started,completed,
            event.payload.to_string(),json!({"eventSeq":seq,"source":"app_server","epoch":event.epoch_id}).to_string(),seq],
    )?;
    store_coverage(tx, &event.thread_key, turn_id, &coverage)?;
    Ok(())
}

fn upsert_turn(
    tx: &Transaction<'_>,
    event: &NormalizedEvent,
    seq: i64,
    update: TurnUpdate<'_>,
) -> Result<()> {
    let terminal = matches!(update.status, "completed" | "failed" | "interrupted");
    let mut coverage = load_coverage(tx, &event.thread_key, update.turn_id)?;
    coverage.durable_started |= update.durable_started;
    coverage.durable_terminal |= terminal;
    let (completeness, reasons) = coverage_state(&coverage);
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
        params![event.thread_key, update.turn_id, update.status, completeness, serde_json::to_string(&reasons)?, serde_json::to_string(&coverage)?, update.started, update.completed,
                if event.top_type == "turn_context" { Some(event.payload.to_string()) } else { None },
                event.payload.to_string(), json!({"eventSeq":seq}).to_string(), seq],
    )?;
    store_coverage(tx, &event.thread_key, update.turn_id, &coverage)?;
    Ok(())
}

fn value_as_string(value: Option<&Value>) -> Option<String> {
    value.map(|value| {
        value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string())
    })
}

fn live_status(value: Option<&Value>) -> Option<String> {
    value
        .map(|value| value.get("type").unwrap_or(value))
        .and_then(value_as_string_ref)
}

fn value_as_string_ref(value: &Value) -> Option<String> {
    if value.is_null() {
        None
    } else {
        Some(
            value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string()),
        )
    }
}

fn parse_stored_json(value: String) -> Value {
    serde_json::from_str(&value).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::Importer;
    use tempfile::TempDir;

    fn message_event(event_id: &str, source_seq: i64, summary: &str) -> NormalizedEvent {
        let payload = json!({"type":"agent_message","message":summary});
        NormalizedEvent {
            event_id: event_id.into(),
            source_id: "source".into(),
            store_source_id: "source".into(),
            epoch_id: "epoch".into(),
            source_seq,
            dedupe_key: event_id.into(),
            observed_at_ms: source_seq,
            event_at_ms: Some(source_seq),
            thread_key: "thread-key".into(),
            codex_thread_id: "thread-1".into(),
            turn_id: Some("turn-1".into()),
            item_id: Some("item-1".into()),
            request_id: None,
            blob_id: None,
            protocol_direction: None,
            worker_id: None,
            worker_connection_epoch: None,
            proxy_seq: None,
            method: "item/completed".into(),
            phase: "completed".into(),
            durability: "durable".into(),
            projectable: true,
            source_fingerprint: "fingerprint".into(),
            stored_raw_hash: blake3::hash(payload.to_string().as_bytes())
                .to_hex()
                .to_string(),
            raw_json: payload.to_string(),
            redaction_json: "{}".into(),
            decode_status: "decoded".into(),
            decode_error: None,
            top_type: "event_msg".into(),
            item_type: Some("agent_message".into()),
            item_status: Some("completed".into()),
            summary_text: Some(summary.into()),
            payload,
        }
    }

    fn protocol_request_event(
        event_id: &str,
        source_seq: i64,
        request_id: &str,
        method: &str,
        direction: &str,
    ) -> NormalizedEvent {
        let payload = json!({"threadId":"thread-1"});
        NormalizedEvent {
            event_id: event_id.into(),
            source_id: "source".into(),
            store_source_id: "source".into(),
            epoch_id: "epoch".into(),
            source_seq,
            dedupe_key: event_id.into(),
            observed_at_ms: source_seq,
            event_at_ms: Some(source_seq),
            thread_key: "thread-key".into(),
            codex_thread_id: "thread-1".into(),
            turn_id: None,
            item_id: None,
            request_id: Some(request_id.into()),
            blob_id: None,
            protocol_direction: Some(direction.into()),
            worker_id: Some("worker-1".into()),
            worker_connection_epoch: Some("connection-1".into()),
            proxy_seq: Some(source_seq),
            method: method.into(),
            phase: "request".into(),
            durability: "transient".into(),
            projectable: true,
            source_fingerprint: format!("fingerprint-{event_id}"),
            stored_raw_hash: blake3::hash(payload.to_string().as_bytes())
                .to_hex()
                .to_string(),
            raw_json: payload.to_string(),
            redaction_json: "{}".into(),
            decode_status: "decoded".into(),
            decode_error: None,
            top_type: "app_server_proxy".into(),
            item_type: None,
            item_status: None,
            summary_text: None,
            payload,
        }
    }

    #[test]
    fn only_upstream_server_requests_enter_pending_request_projection() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        database.upsert_source("source", "fixture", &json!({}), "ready")?;
        database.ingest_batch(&OwnedIngestBatch {
            source_id: "source".into(),
            epoch_id: "epoch".into(),
            checkpoint_key: "proxy:worker-1:connection-1".into(),
            file_identity: "worker:worker-1".into(),
            byte_offset: 0,
            ordinal: 2,
            current_turn_id: None,
            clean_eof: false,
            events: vec![
                protocol_request_event("client-request", 1, "1", "thread/read", "tui_to_upstream"),
                protocol_request_event(
                    "server-request",
                    2,
                    "2",
                    "item/tool/requestUserInput",
                    "upstream_to_tui",
                ),
            ],
        })?;

        let connection = database.connect()?;
        let client_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pending_requests WHERE request_id='1'",
            [],
            |row| row.get(0),
        )?;
        let server: (i64, String, String) = connection.query_row(
            "SELECT COUNT(*),request_type,state FROM pending_requests WHERE request_id='2'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(client_count, 0);
        assert_eq!(server, (1, "user_input".into(), "pending".into()));
        Ok(())
    }

    #[test]
    fn migration_twenty_removes_legacy_client_requests_from_pending_projection() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        database.upsert_source("source", "fixture", &json!({}), "ready")?;
        database.ingest_batch(&OwnedIngestBatch {
            source_id: "source".into(),
            epoch_id: "epoch".into(),
            checkpoint_key: "proxy:worker-1:connection-1".into(),
            file_identity: "worker:worker-1".into(),
            byte_offset: 0,
            ordinal: 1,
            current_turn_id: None,
            clean_eof: false,
            events: vec![protocol_request_event(
                "client-request",
                1,
                "1",
                "thread/read",
                "tui_to_upstream",
            )],
        })?;
        let connection = database.connect()?;
        let request_event_seq: i64 = connection.query_row(
            "SELECT event_seq FROM raw_events WHERE dedupe_key='client-request'",
            [],
            |row| row.get(0),
        )?;
        connection.execute(
            "INSERT INTO pending_requests(source_id,epoch_id,request_id,thread_key,request_type,
               state,request_event_seq,payload_json)
             VALUES ('source','epoch','1','thread-key','unknown','pending',?1,'{}')",
            [request_event_seq],
        )?;
        connection.pragma_update(None, "user_version", 19)?;
        drop(connection);

        database.migrate()?;
        database.migrate()?;
        let connection = database.connect()?;
        let pending_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pending_requests WHERE request_id='1'",
            [],
            |row| row.get(0),
        )?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(pending_count, 0);
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn database_and_data_directory_are_private_on_disk() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = TempDir::new()?;
        let data = temp.path().join("observer-data");
        fs::create_dir(&data)?;
        fs::set_permissions(&data, fs::Permissions::from_mode(0o755))?;
        let path = data.join("custom.database");
        let database = Database::open(&path)?;
        database.migrate()?;
        assert_eq!(fs::metadata(&data)?.permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);

        let target = temp.path().join("target.sqlite");
        fs::write(&target, b"")?;
        let link = data.join("linked.sqlite");
        symlink(&target, &link)?;
        assert!(Database::open(&link).is_err());
        Ok(())
    }

    fn version_four_database(temp: &TempDir) -> Result<Database> {
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        let connection = database.connect()?;
        connection.execute_batch(MIGRATION_1)?;
        connection.execute_batch(MIGRATION_2)?;
        connection.execute_batch(MIGRATION_3)?;
        connection.execute_batch(MIGRATION_4)?;
        drop(connection);
        Ok(database)
    }

    #[test]
    fn durable_turn_aborted_is_an_interrupted_terminal_turn() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        database.upsert_source("source", "fixture", &json!({}), "ready")?;
        let event = |id: &str, source_seq: i64, nested_type: &str| {
            let payload = json!({"type":nested_type,"turn_id":"turn-1"});
            NormalizedEvent {
                event_id: id.into(),
                source_id: "source".into(),
                store_source_id: "source".into(),
                epoch_id: "epoch".into(),
                source_seq,
                dedupe_key: id.into(),
                observed_at_ms: source_seq,
                event_at_ms: Some(source_seq),
                thread_key: "thread-key".into(),
                codex_thread_id: "thread-1".into(),
                turn_id: Some("turn-1".into()),
                item_id: None,
                request_id: None,
                blob_id: None,
                protocol_direction: None,
                worker_id: None,
                worker_connection_epoch: None,
                proxy_seq: None,
                method: format!("event/{nested_type}"),
                phase: if nested_type == "task_started" {
                    "started"
                } else {
                    "completed"
                }
                .into(),
                durability: "durable".into(),
                projectable: true,
                source_fingerprint: "fingerprint".into(),
                stored_raw_hash: blake3::hash(payload.to_string().as_bytes())
                    .to_hex()
                    .to_string(),
                raw_json: payload.to_string(),
                redaction_json: "{}".into(),
                decode_status: "decoded".into(),
                decode_error: None,
                top_type: "event_msg".into(),
                item_type: None,
                item_status: None,
                summary_text: None,
                payload,
            }
        };
        database.ingest_batch(&OwnedIngestBatch {
            source_id: "source".into(),
            epoch_id: "epoch".into(),
            checkpoint_key: "fixture".into(),
            file_identity: "fixture".into(),
            byte_offset: 2,
            ordinal: 2,
            current_turn_id: Some("turn-1".into()),
            clean_eof: true,
            events: vec![
                event("started", 1, "task_started"),
                event("aborted", 2, "turn_aborted"),
            ],
        })?;

        let connection = database.connect()?;
        let (status, completeness, reasons): (String, String, String) = connection.query_row(
            "SELECT status,capture_completeness,completeness_reasons_json FROM turns
             WHERE thread_key='thread-key' AND turn_id='turn-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(status, "interrupted");
        assert_eq!(completeness, "durable_complete");
        assert_eq!(reasons, "[]");

        let mut coverage = load_coverage(&connection, "thread-key", "turn-1")?;
        coverage.durable_terminal = false;
        connection.execute(
            "UPDATE turns SET coverage_json=?1,capture_completeness='durable_partial',
             completeness_reasons_json='[\"durable_turn_not_terminal\"]'
             WHERE thread_key='thread-key' AND turn_id='turn-1'",
            [serde_json::to_string(&coverage)?],
        )?;
        recompute_all_completeness(&connection)?;
        let recomputed: String = connection.query_row(
            "SELECT capture_completeness FROM turns WHERE thread_key='thread-key' AND turn_id='turn-1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(recomputed, "durable_complete");
        Ok(())
    }

    #[test]
    fn migrations_upgrade_version_four_idempotently() -> Result<()> {
        let temp = TempDir::new()?;
        let database = version_four_database(&temp)?;
        database.migrate()?;
        database.migrate()?;
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let blob_table: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='blobs')",
            [],
            |row| row.get(0),
        )?;
        let relation_column: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('threads') WHERE name='parent_thread_key')",
            [],
            |row| row.get(0),
        )?;
        let purge_table: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='purged_threads')",
            [],
            |row| row.get(0),
        )?;
        let search_lookup_index: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='index' AND name='items_search_lookup')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        assert!(blob_table);
        assert!(relation_column);
        assert!(purge_table);
        assert!(search_lookup_index);
        Ok(())
    }

    #[test]
    fn migration_fourteen_adds_append_only_command_and_request_cas_schema() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        database.migrate()?;
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        for table in [
            "gateway_commands",
            "command_transitions",
            "control_audit",
            "image_uploads",
        ] {
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )?;
            assert!(exists, "missing migration table {table}");
        }
        for column in [
            "request_version",
            "resolving_command_id",
            "resolving_started_at_ms",
        ] {
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('pending_requests') WHERE name=?1)",
                [column],
                |row| row.get(0),
            )?;
            assert!(exists, "missing pending request CAS column {column}");
        }

        connection.execute(
            "INSERT INTO gateway_commands(command_id,principal_id,capability,idempotency_key,
               payload_hash,source_id,source_epoch,input_summary_json,state,created_at_ms,updated_at_ms)
             VALUES ('command','local_bearer','turn.start','key','hash','source','epoch','{}','received',1,1)",
            [],
        )?;
        connection.execute(
            "INSERT INTO command_transitions(command_id,from_state,to_state,occurred_at_ms,details_summary_json)
             VALUES ('command',NULL,'received',1,'{}')",
            [],
        )?;
        connection.execute(
            "INSERT INTO control_audit(command_id,principal_id,capability,source_id,source_epoch,
               decision,outcome,payload_hash,input_summary_json,occurred_at_ms)
             VALUES ('command','local_bearer','turn.start','source','epoch','received','pending','hash','{}',1)",
            [],
        )?;
        assert!(
            connection
                .execute(
                    "UPDATE command_transitions SET to_state='failed' WHERE command_id='command'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute("DELETE FROM control_audit WHERE command_id='command'", [])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn migration_fourteen_rolls_back_all_ddl_on_late_failure() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        let connection = database.connect()?;
        for (version, migration) in schema::migrations() {
            if version < 14 {
                connection.execute_batch(migration)?;
            }
        }
        connection.execute("CREATE TABLE control_audit(conflict INTEGER)", [])?;
        drop(connection);

        assert!(database.migrate().is_err());
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let gateway_table: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='gateway_commands')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, 13);
        assert!(!gateway_table);
        Ok(())
    }

    #[test]
    fn migration_fifteen_adds_rebuildable_thread_goal_projection() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        database.migrate()?;
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='thread_goals')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, schema::LATEST_SCHEMA_VERSION);
        assert!(exists);
        Ok(())
    }

    #[test]
    fn migration_fifteen_rolls_back_on_late_failure() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        let connection = database.connect()?;
        for (version, migration) in schema::migrations() {
            if version < 15 {
                connection.execute_batch(migration)?;
            }
        }
        connection.execute(
            "CREATE INDEX thread_goals_source_epoch ON threads(thread_key)",
            [],
        )?;
        drop(connection);

        assert!(database.migrate().is_err());
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let table_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='thread_goals')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, 14);
        assert!(!table_exists);
        Ok(())
    }

    #[test]
    fn migration_sixteen_adds_proxy_provenance_schema_idempotently() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        database.migrate()?;
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, schema::LATEST_SCHEMA_VERSION);
        for column in [
            "protocol_direction",
            "worker_id",
            "worker_connection_epoch",
            "proxy_seq",
        ] {
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('raw_events') WHERE name=?1)",
                [column],
                |row| row.get(0),
            )?;
            assert!(exists, "missing Slice 4 raw provenance column {column}");
        }
        let origin_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('gateway_commands') WHERE name='origin')",
            [],
            |row| row.get(0),
        )?;
        let connection_table_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='worker_connection_epochs')",
            [],
            |row| row.get(0),
        )?;
        assert!(origin_exists);
        assert!(connection_table_exists);
        Ok(())
    }

    #[test]
    fn migration_sixteen_rolls_back_proxy_columns_on_late_failure() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        let connection = database.connect()?;
        for (version, migration) in schema::migrations() {
            if version < 16 {
                connection.execute_batch(migration)?;
            }
        }
        connection.execute(
            "CREATE INDEX raw_events_worker_connection_seq ON raw_events(event_seq)",
            [],
        )?;
        drop(connection);

        assert!(database.migrate().is_err());
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let proxy_column_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('raw_events') WHERE name='protocol_direction')",
            [],
            |row| row.get(0),
        )?;
        let origin_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('gateway_commands') WHERE name='origin')",
            [],
            |row| row.get(0),
        )?;
        let connection_table_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='worker_connection_epochs')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, 15);
        assert!(!proxy_column_exists);
        assert!(!origin_exists);
        assert!(!connection_table_exists);
        Ok(())
    }

    #[test]
    fn migration_seventeen_adds_exclusive_session_lease_schema_idempotently() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        database.migrate()?;
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, schema::LATEST_SCHEMA_VERSION);
        for table in [
            "session_workers",
            "session_worker_transitions",
            "thread_leases",
            "thread_lease_transitions",
            "terminal_attachments",
            "input_leases",
            "input_lease_transitions",
        ] {
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )?;
            assert!(exists, "missing Slice 5 table {table}");
        }

        connection.execute(
            "INSERT INTO gateway_commands(
               command_id,principal_id,capability,idempotency_key,payload_hash,source_id,source_epoch,
               input_summary_json,origin,state,created_at_ms,updated_at_ms)
             VALUES ('command-1','principal-1','session.create','key-1','hash-1','source-1',
               'epoch-1','{}','worker_control','received',1,1)",
            [],
        )?;
        connection.execute(
            "INSERT INTO session_workers(
               worker_id,create_command_id,principal_id,source_id,source_epoch,mode,state,version,canonical_cwd,rows,cols,
               runtime_dir_name,created_at_ms,updated_at_ms)
             VALUES ('worker-1','command-1','principal-1','source-1','epoch-1','resume','ready',1,'/synthetic',24,80,
               'worker-1',1,1)",
            [],
        )?;
        connection.execute(
            "INSERT INTO thread_leases(
               lease_id,source_id,source_epoch,codex_thread_id,worker_id,role,state,version,
               created_at_ms,updated_at_ms)
             VALUES ('lease-1','source-1','epoch-1','thread-1','worker-1','primary','active',1,1,1)",
            [],
        )?;
        assert!(
            connection
                .execute(
                    "INSERT INTO thread_leases(
                       lease_id,source_id,source_epoch,codex_thread_id,worker_id,role,state,version,
                       created_at_ms,updated_at_ms)
                     VALUES ('lease-2','source-1','epoch-1','thread-1','worker-1','side','active',1,1,1)",
                    [],
                )
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn migration_seventeen_rolls_back_all_session_tables_on_late_failure() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        let connection = database.connect()?;
        for (version, migration) in schema::migrations() {
            if version < 17 {
                connection.execute_batch(migration)?;
            }
        }
        connection.execute("CREATE TABLE input_leases(conflict INTEGER)", [])?;
        drop(connection);

        assert!(database.migrate().is_err());
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let worker_table: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='session_workers')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, 16);
        assert!(!worker_table);
        Ok(())
    }

    #[test]
    fn migration_eighteen_adds_turn_owner_schema_idempotently() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        database.migrate()?;
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, schema::LATEST_SCHEMA_VERSION);
        for table in ["turn_owners", "turn_owner_transitions"] {
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )?;
            assert!(exists, "missing Slice 7 table {table}");
        }
        for trigger in [
            "turn_owner_transitions_no_update",
            "turn_owner_transitions_no_delete",
        ] {
            let exists: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='trigger' AND name=?1)",
                [trigger],
                |row| row.get(0),
            )?;
            assert!(exists, "missing append-only trigger {trigger}");
        }
        Ok(())
    }

    #[test]
    fn migration_eighteen_rolls_back_turn_owner_tables_on_late_failure() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        let connection = database.connect()?;
        for (version, migration) in schema::migrations() {
            if version < 18 {
                connection.execute_batch(migration)?;
            }
        }
        connection.execute(
            "CREATE INDEX turn_owner_transitions_turn ON session_workers(worker_id)",
            [],
        )?;
        drop(connection);

        assert!(database.migrate().is_err());
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let owner_table_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='turn_owners')",
            [],
            |row| row.get(0),
        )?;
        let transitions_table_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='turn_owner_transitions')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, 17);
        assert!(!owner_table_exists);
        assert!(!transitions_table_exists);
        Ok(())
    }

    #[test]
    fn migration_nineteen_rebuilds_search_index_with_stable_item_rowids() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        let connection = database.connect()?;
        for (version, migration) in schema::migrations() {
            if version < 19 {
                connection.execute_batch(migration)?;
            }
        }
        connection.execute_batch(
            "INSERT INTO threads(thread_key,store_source_id,codex_thread_id,archived,
               capture_completeness,completeness_reasons_json,projection_json,provenance_json,last_event_seq)
             VALUES ('thread-key','source','thread-1',0,'durable_complete','[]','{}','{}',1);
             INSERT INTO items(thread_key,turn_scope,item_id,turn_id,item_type,status,summary_text,
               projection_json,provenance_json,last_event_seq)
             VALUES ('thread-key','turn-1','item-1','turn-1','agent_message','completed',
               'original needle','{}','{}',1);
             INSERT INTO search_index(rowid,entity_key,thread_key,item_id,text)
             VALUES (9001,'thread-key:turn-1:item-1','thread-key','item-1','original needle');",
        )?;
        drop(connection);

        database.migrate()?;
        database.migrate()?;
        let mut connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let (item_rowid, search_rowid): (i64, i64) = connection.query_row(
            "SELECT items.rowid,search_index.rowid FROM items JOIN search_index
               ON search_index.entity_key=items.thread_key || ':' || items.turn_scope || ':' || items.item_id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        assert_eq!(item_rowid, search_rowid);
        assert_ne!(search_rowid, 9001);

        let transaction = connection.transaction()?;
        project_event(
            &transaction,
            &message_event("updated", 2, "updated needle"),
            2,
        )?;
        transaction.commit()?;
        let indexed: String = connection.query_row(
            "SELECT text FROM search_index WHERE rowid=?1",
            [item_rowid],
            |row| row.get(0),
        )?;
        let matches: i64 = connection.query_row(
            "SELECT COUNT(*) FROM search_index WHERE search_index MATCH 'updated'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(indexed, "updated needle");
        assert_eq!(matches, 1);

        let transaction = connection.transaction()?;
        project_event(&transaction, &message_event("cleared", 3, ""), 3)?;
        transaction.commit()?;
        let indexed: i64 = connection.query_row(
            "SELECT COUNT(*) FROM search_index WHERE rowid=?1",
            [item_rowid],
            |row| row.get(0),
        )?;
        assert_eq!(indexed, 0);
        Ok(())
    }

    #[test]
    fn migration_nineteen_rolls_back_search_rebuild_on_late_failure() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        let connection = database.connect()?;
        for (version, migration) in schema::migrations() {
            if version < 19 {
                connection.execute_batch(migration)?;
            }
        }
        connection.execute_batch(
            "INSERT INTO threads(thread_key,store_source_id,codex_thread_id,archived,
               capture_completeness,completeness_reasons_json,projection_json,provenance_json,last_event_seq)
             VALUES ('thread-key','source','thread-1',0,'durable_complete','[]','{}','{}',1);
             INSERT INTO items(thread_key,turn_scope,item_id,item_type,status,summary_text,
               projection_json,provenance_json,last_event_seq)
             VALUES ('thread-key','turn-1','item-1','agent_message','completed',
               'original needle','{}','{}',1);
             INSERT INTO search_index(rowid,entity_key,thread_key,item_id,text)
             VALUES (9001,'thread-key:turn-1:item-1','thread-key','item-1','original needle');",
        )?;
        let failing = MIGRATION_19.replace(
            "PRAGMA user_version = 19;",
            "SELECT no_such_function();\nPRAGMA user_version = 19;",
        );
        assert!(connection.execute_batch(&failing).is_err());
        connection.execute_batch("ROLLBACK;")?;

        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let search_rowid: i64 = connection.query_row(
            "SELECT rowid FROM search_index WHERE search_index MATCH 'original'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, 18);
        assert_eq!(search_rowid, 9001);
        Ok(())
    }

    #[test]
    fn migration_five_rolls_back_all_ddl_on_failure() -> Result<()> {
        let temp = TempDir::new()?;
        let database = version_four_database(&temp)?;
        database
            .connect()?
            .execute("CREATE TABLE blobs(conflict INTEGER)", [])?;
        assert!(database.migrate().is_err());
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let blob_column: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('raw_events') WHERE name='blob_id')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, 4);
        assert!(!blob_column);
        Ok(())
    }

    #[test]
    fn migration_six_rolls_back_metadata_columns_on_late_failure() -> Result<()> {
        let temp = TempDir::new()?;
        let database = version_four_database(&temp)?;
        database.connect()?.execute_batch(MIGRATION_5)?;
        database
            .connect()?
            .execute("CREATE INDEX threads_parent ON threads(thread_key)", [])?;
        assert!(database.migrate().is_err());
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let parent_column: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('threads') WHERE name='parent_thread_key')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, 5);
        assert!(!parent_column);
        Ok(())
    }

    #[test]
    fn migration_seven_rolls_back_purge_tables_on_late_failure() -> Result<()> {
        let temp = TempDir::new()?;
        let database = version_four_database(&temp)?;
        let connection = database.connect()?;
        connection.execute_batch(MIGRATION_5)?;
        connection.execute_batch(MIGRATION_6)?;
        connection.execute("CREATE TABLE maintenance_audit(conflict INTEGER)", [])?;
        drop(connection);
        assert!(database.migrate().is_err());
        let connection = database.connect()?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let purge_table: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='purged_threads')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, 6);
        assert!(!purge_table);
        Ok(())
    }

    #[test]
    fn migration_nine_reclassifies_existing_unknown_rollout_events() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        let connection = database.connect()?;
        for migration in [
            MIGRATION_1,
            MIGRATION_2,
            MIGRATION_3,
            MIGRATION_4,
            MIGRATION_5,
            MIGRATION_6,
            MIGRATION_7,
            MIGRATION_8,
        ] {
            connection.execute_batch(migration)?;
        }
        connection.execute(
            "INSERT INTO sources(source_id,kind,stable_identity,config_json,status,created_at_ms,updated_at_ms)
             VALUES ('source','rollout','source','{}','online',0,0)",
            [],
        )?;
        connection.execute(
            "INSERT INTO source_epochs(source_id,epoch_id,opened_at_ms) VALUES ('source','epoch',0)",
            [],
        )?;
        connection.execute(
            "INSERT INTO raw_events(event_id,source_id,epoch_id,source_seq,dedupe_key,observed_at_ms,
               thread_key,codex_thread_id,method,phase,durability,source_fingerprint,stored_raw_hash,
               raw_json,redaction_json,decode_status,store_source_id)
             VALUES ('event','source','epoch',1,'event',0,'thread','thread','rollout/future_variant',
               'completed','durable','hash','hash','{}','{}','decoded','source')",
            [],
        )?;
        drop(connection);
        database.migrate()?;
        let connection = database.connect()?;
        let (status, version): (String, i64) = connection.query_row(
            "SELECT decode_status,(SELECT user_version FROM pragma_user_version) FROM raw_events WHERE event_id='event'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(status, "unknown");
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        Ok(())
    }

    #[test]
    fn orphan_sweeper_removes_only_files_without_database_rows() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let orphan = temp.path().join("blobs/orphan.json");
        fs::write(&orphan, "orphan")?;
        assert_eq!(database.sweep_orphan_blobs(0)?, 1);
        assert!(!orphan.exists());

        let referenced = temp.path().join("blobs/referenced.json");
        fs::write(&referenced, "referenced")?;
        database.connect()?.execute(
            "INSERT INTO blobs(blob_id,stored_hash,media_type,size_bytes,relative_path,created_event_seq,created_at_ms)
             VALUES ('blob_ref','hash','application/json',10,'referenced.json',1,0)",
            [],
        )?;
        assert_eq!(database.sweep_orphan_blobs(0)?, 0);
        assert!(referenced.exists());
        Ok(())
    }

    #[test]
    fn doctor_is_read_only_and_reports_missing_or_current_database() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = Config::default();
        config.storage.database = temp.path().join("missing/observer.sqlite");
        config.sources[0].codex_home = temp.path().join("codex-home");
        fs::create_dir_all(config.sources[0].codex_home.join("sessions"))?;

        let report = Database::doctor_read_only(&config)?;
        assert_eq!(report.status, "degraded");
        assert_eq!(report.database, "missing");
        assert!(!temp.path().join("missing").exists());

        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        let report = Database::doctor_read_only(&config)?;
        assert_eq!(report.status, "healthy");
        assert_eq!(
            report.database,
            format!("ok; schema_version={LATEST_SCHEMA_VERSION}")
        );
        Ok(())
    }

    #[test]
    fn export_then_purge_is_redacted_audited_and_not_reimported() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = Config::default();
        config.sources[0].codex_home =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex-home");
        config.storage.database = temp.path().join("observer.sqlite");
        config.storage.blob_dir = temp.path().join("blobs");
        config.storage.fingerprint_key_file = temp.path().join("fingerprint.key");
        let database = Database::open_with_blobs(
            &config.storage.database,
            &config.storage.blob_dir,
            config.capture.inline_blob_bytes,
        )?;
        database.migrate()?;
        Importer::new(&config, &database)?.import_all()?;
        let thread_key: String = database.connect()?.query_row(
            "SELECT thread_key FROM threads WHERE codex_thread_id='00000000-0000-7000-8000-000000000001'",
            [],
            |row| row.get(0),
        )?;

        let output = temp.path().join("thread-export.json");
        let exported = database.export_thread(&thread_key, &output)?;
        assert!(exported.raw_events > 0);
        assert!(exported.turns > 0);
        let contents = fs::read_to_string(&output)?;
        assert!(contents.contains("codex-local-observer-export-v1"));
        assert!(!contents.contains("fixture-secret-must-be-redacted"));
        assert!(!contents.contains("Bearer fixture-secret"));
        assert!(database.export_thread(&thread_key, &output).is_err());

        let purged = database.purge_thread(&thread_key)?;
        assert!(purged.deleted_raw_events > 0);
        assert!(purged.suppression_tombstone);
        let connection = database.connect()?;
        let thread_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM threads WHERE thread_key=?1)",
            [&thread_key],
            |row| row.get(0),
        )?;
        let audit: String = connection.query_row(
            "SELECT details_json FROM maintenance_audit WHERE audit_id=?1",
            [&purged.audit_id],
            |row| row.get(0),
        )?;
        assert!(!thread_exists);
        assert!(!audit.contains("fixture-secret"));
        connection.execute("DELETE FROM source_checkpoints", [])?;
        drop(connection);

        Importer::new(&config, &database)?.import_all()?;
        let thread_exists: bool = database.connect()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM threads WHERE thread_key=?1)",
            [&thread_key],
            |row| row.get(0),
        )?;
        assert!(!thread_exists);
        Ok(())
    }

    #[test]
    fn migration_twelve_backfills_project_and_context_idempotently() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = Config::default();
        config.sources[0].codex_home =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex-home");
        config.storage.database = temp.path().join("observer.sqlite");
        config.storage.fingerprint_key_file = temp.path().join("fingerprint.key");
        let database = Database::open(&config.storage.database)?;
        database.migrate()?;
        Importer::new(&config, &database)?.import_all()?;
        let connection = database.connect()?;
        connection.execute(
            "UPDATE threads SET project_key=NULL,base_instructions_json=NULL,
             dynamic_tools_json=NULL,selected_capability_roots_json=NULL,memory_mode=NULL,
             subagent_history_start_ordinal=NULL,multi_agent_version=NULL,context_window_json=NULL",
            [],
        )?;
        drop(connection);

        database.migrate()?;
        let connection = database.connect()?;
        let project_key_value: String = connection.query_row(
            "SELECT project_key FROM threads WHERE codex_thread_id='00000000-0000-7000-8000-000000000001'",
            [],
            |row| row.get(0),
        )?;
        let base_instructions: Option<String> = connection.query_row(
            "SELECT base_instructions_json FROM threads WHERE codex_thread_id='00000000-0000-7000-8000-000000000001'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(project_key_value, "/demo/local-observer");
        assert!(
            base_instructions
                .unwrap_or_default()
                .contains("Keep changes read-only.")
        );
        drop(connection);

        database.migrate()?;
        let connection = database.connect()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM threads WHERE codex_thread_id='00000000-0000-7000-8000-000000000001'
             AND project_key='/demo/local-observer'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
        Ok(())
    }

    #[test]
    fn migration_thirteen_removes_generated_codex_desktop_projects() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        let connection = database.connect()?;
        for migration in [
            MIGRATION_1,
            MIGRATION_2,
            MIGRATION_3,
            MIGRATION_4,
            MIGRATION_5,
            MIGRATION_6,
            MIGRATION_7,
            MIGRATION_8,
            MIGRATION_9,
            MIGRATION_10,
            MIGRATION_11,
            MIGRATION_12,
        ] {
            connection.execute_batch(migration)?;
        }
        connection.execute(
            "INSERT INTO threads(thread_key,store_source_id,codex_thread_id,cwd,originator,project_key,last_event_seq)
             VALUES ('projectless','source','projectless','/Users/demo/Documents/Codex/2026-08-28/generated-name',
               'Codex Desktop','/Users/demo/Documents/Codex/2026-08-28/generated-name',0),
              ('project','source','project','/Users/demo/workspace/project','Codex Desktop',
               '/Users/demo/workspace/project',0),
              ('work-desktop','source','work-desktop','/Users/demo/Documents/Codex/2026-08-27/generated-work',
               'codex_work_desktop','/Users/demo/Documents/Codex/2026-08-27/generated-work',0),
              ('unknown','source','unknown','','Codex Desktop','unknown',0)",
            [],
        )?;
        drop(connection);

        database.migrate()?;
        database.migrate()?;
        let connection = database.connect()?;
        let projectless: Option<String> = connection.query_row(
            "SELECT project_key FROM threads WHERE thread_key='projectless'",
            [],
            |row| row.get(0),
        )?;
        let project: Option<String> = connection.query_row(
            "SELECT project_key FROM threads WHERE thread_key='project'",
            [],
            |row| row.get(0),
        )?;
        let work_desktop: Option<String> = connection.query_row(
            "SELECT project_key FROM threads WHERE thread_key='work-desktop'",
            [],
            |row| row.get(0),
        )?;
        let unknown: Option<String> = connection.query_row(
            "SELECT project_key FROM threads WHERE thread_key='unknown'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(projectless, None);
        assert_eq!(work_desktop, None);
        assert_eq!(unknown, None);
        assert_eq!(project.as_deref(), Some("/Users/demo/workspace/project"));
        Ok(())
    }
}
