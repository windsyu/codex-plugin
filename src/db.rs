use std::collections::HashSet;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

use crate::config::Config;
use crate::ingest::thread_key;
use crate::model::{
    Checkpoint, DoctorReport, DoctorSource, ExportReport, NormalizedEvent, PurgeReport,
    RetentionReport,
};

const MIGRATION_1: &str = include_str!("../migrations/0001_initial.sql");
const MIGRATION_2: &str = include_str!("../migrations/0002_fts_trigram.sql");
const MIGRATION_3: &str = include_str!("../migrations/0003_retention.sql");
const MIGRATION_4: &str = include_str!("../migrations/0004_live_sources.sql");
const MIGRATION_5: &str = include_str!("../migrations/0005_blobs.sql");
const MIGRATION_6: &str = include_str!("../migrations/0006_thread_metadata.sql");
const MIGRATION_7: &str = include_str!("../migrations/0007_local_purge.sql");

#[derive(Debug)]
pub struct Database {
    path: PathBuf,
    blob_dir: PathBuf,
    inline_blob_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct BlobRecord {
    pub blob_id: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub path: PathBuf,
}

struct PreparedBlob {
    blob_id: String,
    stored_hash: String,
    media_type: &'static str,
    size_bytes: usize,
    relative_path: String,
    redaction_json: String,
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
            fs::create_dir_all(parent)
                .with_context(|| format!("create database directory {}", parent.display()))?;
        }
        let database = Self {
            path: path.to_path_buf(),
            blob_dir: blob_dir.to_path_buf(),
            inline_blob_bytes,
        };
        database.initialize_blob_dir()?;
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

    fn initialize_blob_dir(&self) -> Result<()> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&self.blob_dir)
            .with_context(|| format!("create blob directory {}", self.blob_dir.display()))?;
        let metadata = fs::symlink_metadata(&self.blob_dir)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            anyhow::bail!("blob_dir must be a direct directory");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
                anyhow::bail!("blob_dir must be owned by the current user with mode 0700");
            }
        }
        Ok(())
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
            .connect()?
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
        let connection = self.connect()?;
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
            connection
                .execute_batch(MIGRATION_4)
                .context("apply migration 0004")?;
            connection
                .execute_batch(MIGRATION_5)
                .context("apply migration 0005")?;
            connection
                .execute_batch(MIGRATION_6)
                .context("apply migration 0006")?;
            connection
                .execute_batch(MIGRATION_7)
                .context("apply migration 0007")?;
        } else if version == 1 {
            connection
                .execute_batch(MIGRATION_2)
                .context("apply migration 0002")?;
            connection
                .execute_batch(MIGRATION_3)
                .context("apply migration 0003")?;
            connection
                .execute_batch(MIGRATION_4)
                .context("apply migration 0004")?;
            connection
                .execute_batch(MIGRATION_5)
                .context("apply migration 0005")?;
            connection
                .execute_batch(MIGRATION_6)
                .context("apply migration 0006")?;
            connection
                .execute_batch(MIGRATION_7)
                .context("apply migration 0007")?;
        } else if version == 2 {
            connection
                .execute_batch(MIGRATION_3)
                .context("apply migration 0003")?;
            connection
                .execute_batch(MIGRATION_4)
                .context("apply migration 0004")?;
            connection
                .execute_batch(MIGRATION_5)
                .context("apply migration 0005")?;
            connection
                .execute_batch(MIGRATION_6)
                .context("apply migration 0006")?;
            connection
                .execute_batch(MIGRATION_7)
                .context("apply migration 0007")?;
        } else if version == 3 {
            connection
                .execute_batch(MIGRATION_4)
                .context("apply migration 0004")?;
            connection
                .execute_batch(MIGRATION_5)
                .context("apply migration 0005")?;
            connection
                .execute_batch(MIGRATION_6)
                .context("apply migration 0006")?;
            connection
                .execute_batch(MIGRATION_7)
                .context("apply migration 0007")?;
        } else if version == 4 {
            connection
                .execute_batch(MIGRATION_5)
                .context("apply migration 0005")?;
            connection
                .execute_batch(MIGRATION_6)
                .context("apply migration 0006")?;
            connection
                .execute_batch(MIGRATION_7)
                .context("apply migration 0007")?;
        } else if version == 5 {
            connection
                .execute_batch(MIGRATION_6)
                .context("apply migration 0006")?;
            connection
                .execute_batch(MIGRATION_7)
                .context("apply migration 0007")?;
        } else if version == 6 {
            connection
                .execute_batch(MIGRATION_7)
                .context("apply migration 0007")?;
        } else if version != 7 {
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
        let now = now_ms();
        self.connect()?.execute(
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

    pub fn ingest_batch(&self, batch: IngestBatch<'_>) -> Result<(usize, usize)> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
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
        let mut last_event_seq: Option<i64> = None;
        for event in batch.events {
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
                   source_fingerprint,stored_raw_hash,raw_json,redaction_json,decode_status,decode_error,request_id,store_source_id,blob_id)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)",
                params![
                    stored_event.event_id, stored_event.source_id, stored_event.epoch_id, stored_event.source_seq,
                    stored_event.dedupe_key, stored_event.observed_at_ms, stored_event.event_at_ms,
                    stored_event.thread_key, stored_event.codex_thread_id, stored_event.turn_id, stored_event.item_id,
                    stored_event.method, stored_event.phase, stored_event.durability, stored_event.source_fingerprint, stored_event.stored_raw_hash,
                    stored_event.raw_json, stored_event.redaction_json, stored_event.decode_status, stored_event.decode_error,
                    stored_event.request_id, stored_event.store_source_id, stored_event.blob_id,
                ],
            )?;
            let event_seq = transaction.last_insert_rowid();
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
                project_event(&transaction, &stored_event, event_seq)?;
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
        let connection = self.connect()?;
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

    pub fn record_live_capabilities(
        &self,
        source_id: &str,
        epoch_id: &str,
        capabilities: &Value,
    ) -> Result<()> {
        let capability_json = capabilities.to_string();
        let capability_hash = blake3::hash(capability_json.as_bytes())
            .to_hex()
            .to_string();
        self.connect()?.execute(
            "INSERT INTO source_epochs(source_id,epoch_id,opened_at_ms,capability_json,capability_hash)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(source_id,epoch_id) DO UPDATE SET capability_json=excluded.capability_json,
               capability_hash=excluded.capability_hash",
            params![source_id, epoch_id, now_ms(), capability_json, capability_hash],
        )?;
        Ok(())
    }

    pub fn update_source_status(
        &self,
        source_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        self.connect()?.execute(
            "UPDATE sources SET status=?1,last_error_json=?2,updated_at_ms=?3 WHERE source_id=?4",
            params![
                status,
                error.map(|message| json!({"message":message}).to_string()),
                now_ms(),
                source_id
            ],
        )?;
        Ok(())
    }

    pub fn close_live_epoch(&self, source_id: &str, epoch_id: &str, reason: &str) -> Result<()> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE source_epochs SET closed_at_ms=?1,close_reason=?2 WHERE source_id=?3 AND epoch_id=?4",
            params![now_ms(), reason, source_id, epoch_id],
        )?;
        transaction.execute(
            "UPDATE threads SET runtime_status_stale=1,
               completeness_reasons_json='[\"source_disconnected\"]'
             WHERE thread_key IN (SELECT DISTINCT thread_key FROM raw_events WHERE source_id=?1 AND epoch_id=?2 AND thread_key<>'')",
            params![source_id, epoch_id],
        )?;
        transaction.execute(
            "UPDATE pending_requests SET state='source_disconnected'
             WHERE source_id=?1 AND epoch_id=?2 AND state='pending'",
            params![source_id, epoch_id],
        )?;
        transaction.commit()?;
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

    pub fn run_retention(
        &self,
        retention_days: u64,
        blob_retention_days: u64,
        apply: bool,
    ) -> Result<RetentionReport> {
        let retention_ms = retention_days
            .checked_mul(86_400_000)
            .context("raw retention duration overflow")? as i64;
        let cutoff = now_ms().saturating_sub(retention_ms);
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
            "SELECT COUNT(*),MAX(event_seq) FROM raw_events WHERE observed_at_ms < ?1",
            [cutoff],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let candidate_blobs: i64 = connection.query_row(
            "SELECT COUNT(*) FROM blobs b WHERE b.created_at_ms < ?1 AND NOT EXISTS (
               SELECT 1 FROM blob_references r WHERE r.blob_id=b.blob_id AND (
                 r.reference_kind='projection' OR (r.reference_kind='raw_event' AND EXISTS (
                   SELECT 1 FROM raw_events e WHERE e.event_seq=r.event_seq AND e.observed_at_ms>=?2
                 ))
               )
             )",
            params![blob_cutoff, cutoff],
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
             (SELECT event_seq FROM raw_events WHERE observed_at_ms < ?1)",
            [cutoff],
        )?;
        transaction.execute(
            "DELETE FROM blob_references WHERE reference_kind='raw_event' AND (
               event_seq IN (SELECT event_seq FROM raw_events WHERE observed_at_ms < ?1)
               OR NOT EXISTS (SELECT 1 FROM raw_events e WHERE e.event_seq=blob_references.event_seq)
             )",
            [cutoff],
        )?;
        let deleted =
            transaction.execute("DELETE FROM raw_events WHERE observed_at_ms < ?1", [cutoff])?;
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
        let connection = self.connect()?;
        let thread = connection
            .query_row(
                "SELECT thread_key,codex_thread_id,store_source_id,name,cwd,source,model,archived,
                   capture_completeness,completeness_reasons_json,created_at_ms,updated_at_ms,recency_at_ms,
                   projection_json,provenance_json,last_event_seq FROM threads WHERE thread_key=?1",
                [thread_key],
                |row| {
                    Ok(json!({
                        "threadKey":row.get::<_,String>(0)?,"codexThreadId":row.get::<_,String>(1)?,
                        "storeSourceId":row.get::<_,String>(2)?,"name":row.get::<_,Option<String>>(3)?,
                        "cwd":row.get::<_,Option<String>>(4)?,"source":row.get::<_,Option<String>>(5)?,
                        "model":row.get::<_,Option<String>>(6)?,"archived":row.get::<_,bool>(7)?,
                        "captureCompleteness":row.get::<_,String>(8)?,
                        "completenessReasons":parse_stored_json(row.get::<_,String>(9)?),
                        "createdAtMs":row.get::<_,Option<i64>>(10)?,"updatedAtMs":row.get::<_,Option<i64>>(11)?,
                        "recencyAtMs":row.get::<_,Option<i64>>(12)?,
                        "projection":parse_stored_json(row.get::<_,String>(13)?),
                        "provenance":parse_stored_json(row.get::<_,String>(14)?),
                        "lastEventSeq":row.get::<_,i64>(15)?
                    }))
                },
            )
            .optional()?
            .with_context(|| format!("thread {thread_key} was not found"))?;
        let turns = self.query_json(
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
        let items = self.query_json(
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
        let mut events = self.query_json(
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
        let export = json!({
            "format":"codex-local-observer-export-v1","exportedAtMs":now_ms(),
            "thread":thread,"turns":turns,"items":items,"rawEvents":events
        });
        write_private_new(output, &serde_json::to_vec_pretty(&export)?)?;
        Ok(ExportReport {
            thread_key: thread_key.to_string(),
            output: output.display().to_string(),
            turns: turns.len(),
            items: items.len(),
            raw_events: events.len(),
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
        transaction.execute("DELETE FROM search_index WHERE thread_key=?1", [thread_key])?;
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
        let (database, mut degraded) = if database_path.is_file() {
            match Connection::open_with_flags(database_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
                Ok(connection) => {
                    connection.busy_timeout(std::time::Duration::from_secs(5))?;
                    let integrity = connection
                        .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                        .unwrap_or_else(|error| format!("unreadable: {error}"));
                    let version = connection
                        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                        .unwrap_or(-1);
                    let healthy = integrity == "ok" && version == 7;
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
            "DELETE FROM blob_references WHERE reference_kind='projection';
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
        params![event.thread_key, event.store_source_id, event.codex_thread_id, time,
                json!({"lastEventSeq":event_seq,"sourceId":event.source_id}).to_string(), event_seq],
    )?;

    if event.durability == "transient" {
        project_live_lifecycle(transaction, event, event_seq, time)?;
    }

    match event.top_type.as_str() {
        "session_meta" => {
            let p = &event.payload;
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
            let session_projection = json!({
                "sessionId":p.get("session_id").or_else(|| p.get("id")),
                "parentThreadId":parent_thread_id,"forkedFromId":forked_from_id,
                "cwd":p.get("cwd"),"source":p.get("source"),"threadSource":p.get("thread_source"),
                "originator":p.get("originator"),"cliVersion":p.get("cli_version"),
                "agentNickname":p.get("agent_nickname"),"agentRole":p.get("agent_role"),"agentPath":p.get("agent_path"),
                "modelProvider":p.get("model_provider"),"historyMode":p.get("history_mode"),"historyBase":history_base
            });
            transaction.execute(
                "UPDATE threads SET session_id=?1,cwd=?2,source=?3,model_provider=?4,created_at_ms=COALESCE(?5,created_at_ms),
                   parent_thread_id=?6,parent_thread_key=?7,forked_from_id=?8,forked_from_thread_key=?9,
                   agent_nickname=?10,agent_role=?11,agent_path=?12,originator=?13,cli_version=?14,
                   thread_source=?15,history_mode=?16,history_base_json=?17,
                   projection_json=?18,provenance_json=?19,last_event_seq=?20 WHERE thread_key=?21",
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
                        "UPDATE threads SET capture_completeness='durable_complete',completeness_reasons_json='[]',runtime_status='idle' WHERE thread_key=?1",
                        [&event.thread_key],
                    )?;
            }
        }
        _ => {}
    }

    if event.top_type != "session_meta" && event.durability == "durable" {
        transaction.execute(
            "UPDATE threads SET capture_completeness=CASE WHEN capture_completeness='durable_complete' THEN capture_completeness ELSE 'durable_partial' END,
               completeness_reasons_json=CASE WHEN capture_completeness='durable_complete' THEN completeness_reasons_json ELSE '[\"durable_rollout_may_still_be_growing\"]' END
             WHERE thread_key=?1",
            [&event.thread_key],
        )?;
    }

    if event.phase == "request"
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
             ON CONFLICT(source_id,epoch_id,request_id) DO NOTHING",
            params![event.source_id,event.epoch_id,request_id,event.thread_key,request_type,event_seq,event.payload.to_string()],
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
        let indexed_summary = transaction
            .query_row(
                "SELECT summary_text FROM items WHERE thread_key=?1 AND turn_scope=?2 AND item_id=?3",
                params![event.thread_key, turn_scope, item_id],
                |row| row.get::<_, Option<String>>(0),
            )?
            .filter(|text| !text.trim().is_empty());
        if let Some(summary) = indexed_summary.as_deref() {
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

fn projection_reference_key(event: &NormalizedEvent) -> String {
    if let Some(item_id) = event.item_id.as_deref() {
        let turn_scope = event
            .turn_id
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| format!("@unassigned:{}:{}", event.source_id, event.epoch_id));
        format!("item:{}:{turn_scope}:{item_id}", event.thread_key)
    } else if let Some(turn_id) = event.turn_id.as_deref() {
        format!("turn:{}:{turn_id}", event.thread_key)
    } else {
        format!("thread:{}", event.thread_key)
    }
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
        "turn/started" => {
            if let Some(turn_id) = event.turn_id.as_deref() {
                upsert_live_turn(
                    tx, event, event_seq, turn_id, "running", time, None, true, false,
                )?;
                tx.execute(
                    "UPDATE threads SET runtime_status='active',runtime_status_stale=0,
                       capture_completeness='live_partial',completeness_reasons_json='[\"live_turn_not_terminal\"]'
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
                    "UPDATE threads SET runtime_status='idle',runtime_status_stale=0,
                       capture_completeness=?1,completeness_reasons_json=?2 WHERE thread_key=?3",
                    params![
                        if live_started {
                            "live_complete"
                        } else {
                            "live_partial"
                        },
                        if live_started {
                            "[]"
                        } else {
                            "[\"attached_after_turn_started\"]"
                        },
                        event.thread_key
                    ],
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
    let complete = live_started && live_terminal;
    let coverage = json!({
        "liveStarted":live_started,"liveTerminal":live_terminal,"liveEpochContiguous":complete,
        "durableStarted":false,"durableTerminal":false,"durableEofReached":false,
        "decodeErrorCount":0,"unknownEventCount":0,"sourceDisconnectCount":0
    });
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
        params![event.thread_key,turn_id,status,if complete {"live_complete"} else {"live_partial"},
            if complete {"[]"} else {"[\"live_epoch_incomplete\"]"},coverage.to_string(),started,completed,
            event.payload.to_string(),json!({"eventSeq":seq,"source":"app_server","epoch":event.epoch_id}).to_string(),seq],
    )?;
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
        "inter_agent_communication" | "inter_agent_communication_metadata" => {
            Some("sub_agent".into())
        }
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

pub fn classify_live_item(method: &str, item: Option<&Value>) -> Option<String> {
    if method.contains("requestApproval") {
        return Some("approval".into());
    }
    if method.contains("requestUserInput") || method.contains("elicitation/request") {
        return Some("user_question".into());
    }
    let kind = item
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        .or_else(|| {
            method
                .strip_prefix("item/")
                .and_then(|rest| rest.split('/').next())
        })?;
    Some(
        match kind {
            "userMessage" => "user_message",
            "agentMessage" => "agent_message",
            "reasoning" => "reasoning",
            "commandExecution" => "command_execution",
            "fileChange" => "file_change",
            "mcpToolCall" => "mcp_tool_call",
            "collabAgentToolCall" => "sub_agent",
            "webSearch" => "web_search",
            "imageGeneration" => "image_generation",
            "plan" => "plan",
            "error" => "error",
            other => other,
        }
        .to_string(),
    )
}

pub fn live_summary(params: &Value, item: Option<&Value>) -> Option<String> {
    params
        .get("delta")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            item.and_then(|item| summary_text(&json!({"type":"response_item","payload":item})))
        })
}

pub fn summary_text(raw: &Value) -> Option<String> {
    let payload = raw.get("payload")?;
    if let Some(text) = payload
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| payload.get("content").and_then(Value::as_str))
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

fn parse_time_ms(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp_millis())
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn parse_stored_json(value: String) -> Value {
    serde_json::from_str(&value).unwrap_or(Value::Null)
}

fn write_private_new(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("export output must have a parent directory")?;
    if !parent.is_dir() {
        anyhow::bail!("export output directory does not exist");
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options
            .open(path)
            .with_context(|| format!("create export output {}", path.display()))?;
        file.write_all(contents)?;
        file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::Importer;
    use tempfile::TempDir;

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
        assert_eq!(version, 7);
        assert!(blob_table);
        assert!(relation_column);
        assert!(purge_table);
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
        assert_eq!(report.database, "ok; schema_version=7");
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
}
