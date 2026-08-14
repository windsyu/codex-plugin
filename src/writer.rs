use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::db::Database;
use crate::model::OwnedIngestBatch;

#[derive(Debug, Clone, Default)]
pub struct WriterMetrics {
    pub ready: bool,
    pub queue_depth_events: usize,
    pub commits: u64,
    pub failures: u64,
    pub commit_p95_ms: u64,
}

struct SharedMetrics {
    ready: AtomicBool,
    queue_depth_events: AtomicUsize,
    commits: AtomicU64,
    failures: AtomicU64,
    latencies_ms: Mutex<Vec<u64>>,
}

struct EventBudget {
    capacity: usize,
    used: Mutex<usize>,
    available: Condvar,
}

impl EventBudget {
    fn reserve(&self, count: usize) -> Result<()> {
        if count > self.capacity {
            anyhow::bail!(
                "ingest batch of {count} events exceeds queue capacity {}",
                self.capacity
            );
        }
        let mut used = self.used.lock().expect("event budget poisoned");
        while *used + count > self.capacity {
            used = self.available.wait(used).expect("event budget poisoned");
        }
        *used += count;
        Ok(())
    }

    fn release(&self, count: usize) {
        let mut used = self.used.lock().expect("event budget poisoned");
        *used = used.saturating_sub(count);
        self.available.notify_all();
    }
}

enum ControlCommand {
    UpsertSource {
        source_id: String,
        kind: String,
        stable_identity: String,
        config: Value,
        status: String,
        reply: mpsc::Sender<Result<()>>,
    },
    MarkLocation {
        source_id: String,
        thread_id: String,
        path: PathBuf,
        representation: String,
        identity: String,
        archived: bool,
        reply: mpsc::Sender<Result<()>>,
    },
    RecordCapabilities {
        source_id: String,
        epoch_id: String,
        capabilities: Value,
        reply: mpsc::Sender<Result<()>>,
    },
    UpdateSourceStatus {
        source_id: String,
        status: String,
        error: Option<String>,
        reply: mpsc::Sender<Result<()>>,
    },
    CloseEpoch {
        source_id: String,
        epoch_id: String,
        reason: String,
        reply: mpsc::Sender<Result<()>>,
    },
    Shutdown,
}

struct IngestCommand {
    batch: OwnedIngestBatch,
    reserved_events: usize,
    reply: mpsc::Sender<Result<(usize, usize)>>,
}

#[derive(Clone)]
pub struct WriterHandle {
    ingest: mpsc::SyncSender<IngestCommand>,
    control: mpsc::SyncSender<ControlCommand>,
    budget: Arc<EventBudget>,
    metrics: Arc<SharedMetrics>,
    committed: broadcast::Sender<i64>,
}

impl WriterHandle {
    pub fn start(
        database: Arc<Database>,
        ingest_capacity: usize,
        control_capacity: usize,
        consumer_capacity: usize,
    ) -> Result<Self> {
        let mut writer_connection = database.connect()?;
        let (ingest_tx, ingest_rx) = mpsc::sync_channel(ingest_capacity);
        let (control_tx, control_rx) = mpsc::sync_channel(control_capacity);
        let (committed, _) = broadcast::channel(consumer_capacity);
        let budget = Arc::new(EventBudget {
            capacity: ingest_capacity,
            used: Mutex::new(0),
            available: Condvar::new(),
        });
        let metrics = Arc::new(SharedMetrics {
            ready: AtomicBool::new(false),
            queue_depth_events: AtomicUsize::new(0),
            commits: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            latencies_ms: Mutex::new(Vec::new()),
        });
        let worker_budget = budget.clone();
        let worker_metrics = metrics.clone();
        let worker_committed = committed.clone();
        thread::Builder::new()
            .name("observer-db-writer".into())
            .spawn(move || {
                worker_metrics.ready.store(true, Ordering::Release);
                writer_loop(
                    &database,
                    &mut writer_connection,
                    ingest_rx,
                    control_rx,
                    &worker_budget,
                    &worker_metrics,
                    &worker_committed,
                );
                worker_metrics.ready.store(false, Ordering::Release);
            })
            .context("spawn Observer DbWriter")?;
        Ok(Self {
            ingest: ingest_tx,
            control: control_tx,
            budget,
            metrics,
            committed,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<i64> {
        self.committed.subscribe()
    }

    pub fn metrics(&self) -> WriterMetrics {
        let mut latencies = self
            .metrics
            .latencies_ms
            .lock()
            .expect("writer latency metrics poisoned")
            .clone();
        latencies.sort_unstable();
        let p95 = if latencies.is_empty() {
            0
        } else {
            latencies[(latencies.len() * 95 / 100).min(latencies.len() - 1)]
        };
        WriterMetrics {
            ready: self.metrics.ready.load(Ordering::Acquire),
            queue_depth_events: self.metrics.queue_depth_events.load(Ordering::Relaxed),
            commits: self.metrics.commits.load(Ordering::Relaxed),
            failures: self.metrics.failures.load(Ordering::Relaxed),
            commit_p95_ms: p95,
        }
    }

    pub fn ingest(&self, batch: OwnedIngestBatch) -> Result<(usize, usize)> {
        let reserved_events = batch.events.len().max(1);
        self.budget.reserve(reserved_events)?;
        self.metrics
            .queue_depth_events
            .fetch_add(reserved_events, Ordering::Relaxed);
        let (reply_tx, reply_rx) = mpsc::channel();
        if self
            .ingest
            .send(IngestCommand {
                batch,
                reserved_events,
                reply: reply_tx,
            })
            .is_err()
        {
            self.budget.release(reserved_events);
            self.metrics
                .queue_depth_events
                .fetch_sub(reserved_events, Ordering::Relaxed);
            anyhow::bail!("Observer DbWriter is unavailable");
        }
        reply_rx.recv().context("Observer DbWriter stopped")?
    }

    fn control(&self, command: ControlCommand, reply: mpsc::Receiver<Result<()>>) -> Result<()> {
        self.control
            .send(command)
            .context("Observer DbWriter is unavailable")?;
        reply.recv().context("Observer DbWriter stopped")?
    }

    pub fn upsert_source_kind(
        &self,
        source_id: &str,
        kind: &str,
        stable_identity: &str,
        config: &Value,
        status: &str,
    ) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.control(
            ControlCommand::UpsertSource {
                source_id: source_id.into(),
                kind: kind.into(),
                stable_identity: stable_identity.into(),
                config: config.clone(),
                status: status.into(),
                reply: tx,
            },
            rx,
        )
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

    #[allow(clippy::too_many_arguments)]
    pub fn mark_location(
        &self,
        source_id: &str,
        thread_id: &str,
        path: PathBuf,
        representation: &str,
        identity: &str,
        archived: bool,
    ) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.control(
            ControlCommand::MarkLocation {
                source_id: source_id.into(),
                thread_id: thread_id.into(),
                path,
                representation: representation.into(),
                identity: identity.into(),
                archived,
                reply: tx,
            },
            rx,
        )
    }

    pub fn record_live_capabilities(
        &self,
        source_id: &str,
        epoch_id: &str,
        capabilities: &Value,
    ) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.control(
            ControlCommand::RecordCapabilities {
                source_id: source_id.into(),
                epoch_id: epoch_id.into(),
                capabilities: capabilities.clone(),
                reply: tx,
            },
            rx,
        )
    }

    pub fn update_source_status(
        &self,
        source_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.control(
            ControlCommand::UpdateSourceStatus {
                source_id: source_id.into(),
                status: status.into(),
                error: error.map(str::to_string),
                reply: tx,
            },
            rx,
        )
    }

    pub fn close_live_epoch(&self, source_id: &str, epoch_id: &str, reason: &str) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.control(
            ControlCommand::CloseEpoch {
                source_id: source_id.into(),
                epoch_id: epoch_id.into(),
                reason: reason.into(),
                reply: tx,
            },
            rx,
        )
    }
}

fn writer_loop(
    database: &Database,
    connection: &mut rusqlite::Connection,
    ingest: mpsc::Receiver<IngestCommand>,
    control: mpsc::Receiver<ControlCommand>,
    budget: &EventBudget,
    metrics: &SharedMetrics,
    committed: &broadcast::Sender<i64>,
) {
    let mut pending: Option<IngestCommand> = None;
    loop {
        while let Ok(command) = control.try_recv() {
            if execute_control(database, connection, command) {
                return;
            }
        }
        let first = pending
            .take()
            .map_or_else(|| ingest.recv_timeout(Duration::from_millis(50)), Ok);
        match first {
            Ok(command) => {
                let deadline = Instant::now() + Duration::from_millis(50);
                let mut event_count = command.batch.events.len().max(1);
                let mut commands = vec![command];
                while event_count < 100 {
                    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    match ingest.recv_timeout(remaining) {
                        Ok(command) => {
                            let count = command.batch.events.len().max(1);
                            if event_count + count > 100 {
                                pending = Some(command);
                                break;
                            }
                            event_count += count;
                            commands.push(command);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                let started = Instant::now();
                let batches = commands
                    .iter()
                    .map(|command| command.batch.clone())
                    .collect::<Vec<_>>();
                let result = database.ingest_batches_on(connection, &batches);
                if result.is_ok() {
                    metrics.commits.fetch_add(1, Ordering::Relaxed);
                    if let Ok(sequence) = connection.query_row(
                        "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='raw_events'),0)",
                        [],
                        |row| row.get(0),
                    ) {
                        let _ = committed.send(sequence);
                    }
                } else {
                    metrics.failures.fetch_add(1, Ordering::Relaxed);
                }
                let elapsed = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                let mut samples = metrics
                    .latencies_ms
                    .lock()
                    .expect("writer latency metrics poisoned");
                if samples.len() >= 1024 {
                    samples.remove(0);
                }
                samples.push(elapsed);
                drop(samples);
                match result {
                    Ok(results) => {
                        for (command, result) in commands.into_iter().zip(results) {
                            budget.release(command.reserved_events);
                            metrics
                                .queue_depth_events
                                .fetch_sub(command.reserved_events, Ordering::Relaxed);
                            let _ = command.reply.send(Ok(result));
                        }
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        for command in commands {
                            budget.release(command.reserved_events);
                            metrics
                                .queue_depth_events
                                .fetch_sub(command.reserved_events, Ordering::Relaxed);
                            let _ = command.reply.send(Err(anyhow::anyhow!(message.clone())));
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn execute_control(
    database: &Database,
    connection: &mut rusqlite::Connection,
    command: ControlCommand,
) -> bool {
    let (result, reply) = match command {
        ControlCommand::UpsertSource {
            source_id,
            kind,
            stable_identity,
            config,
            status,
            reply,
        } => (
            database.upsert_source_kind_on(
                connection,
                &source_id,
                &kind,
                &stable_identity,
                &config,
                &status,
            ),
            reply,
        ),
        ControlCommand::MarkLocation {
            source_id,
            thread_id,
            path,
            representation,
            identity,
            archived,
            reply,
        } => (
            database.mark_location_on(
                connection,
                &source_id,
                &thread_id,
                &path,
                &representation,
                &identity,
                archived,
            ),
            reply,
        ),
        ControlCommand::RecordCapabilities {
            source_id,
            epoch_id,
            capabilities,
            reply,
        } => (
            database.record_live_capabilities_on(connection, &source_id, &epoch_id, &capabilities),
            reply,
        ),
        ControlCommand::UpdateSourceStatus {
            source_id,
            status,
            error,
            reply,
        } => (
            database.update_source_status_on(connection, &source_id, &status, error.as_deref()),
            reply,
        ),
        ControlCommand::CloseEpoch {
            source_id,
            epoch_id,
            reason,
            reply,
        } => (
            database.close_live_epoch_on(connection, &source_id, &epoch_id, &reason),
            reply,
        ),
        ControlCommand::Shutdown => return true,
    };
    let _ = reply.send(result);
    false
}

impl Drop for WriterHandle {
    fn drop(&mut self) {
        if Arc::strong_count(&self.metrics) == 2 {
            let _ = self.control.try_send(ControlCommand::Shutdown);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn event(source: &str, id: &str) -> crate::model::NormalizedEvent {
        crate::model::NormalizedEvent {
            event_id: id.into(),
            source_id: source.into(),
            store_source_id: source.into(),
            epoch_id: "epoch".into(),
            source_seq: 1,
            dedupe_key: id.into(),
            observed_at_ms: 1,
            event_at_ms: None,
            thread_key: String::new(),
            codex_thread_id: String::new(),
            turn_id: None,
            item_id: None,
            request_id: None,
            blob_id: None,
            method: "test/event".into(),
            phase: "snapshot".into(),
            durability: "durable".into(),
            projectable: false,
            source_fingerprint: "fingerprint".into(),
            stored_raw_hash: blake3::hash(b"{}").to_hex().to_string(),
            raw_json: "{}".into(),
            redaction_json: json!({"ruleVersion":"known-secrets-v2"}).to_string(),
            decode_status: "decoded".into(),
            decode_error: None,
            top_type: "test".into(),
            item_type: None,
            item_status: None,
            summary_text: None,
            payload: Value::Null,
        }
    }

    fn batch(source: &str, id: &str) -> OwnedIngestBatch {
        OwnedIngestBatch {
            source_id: source.into(),
            epoch_id: "epoch".into(),
            checkpoint_key: id.into(),
            file_identity: "fixture".into(),
            byte_offset: 1,
            ordinal: 1,
            current_turn_id: None,
            clean_eof: false,
            events: vec![event(source, id)],
        }
    }

    #[test]
    fn publishes_only_after_successful_commit() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let writer = WriterHandle::start(database, 100, 16, 16)?;
        writer.upsert_source("source", "fixture", &json!({}), "ready")?;
        let mut committed = writer.subscribe();
        assert_eq!(writer.ingest(batch("source", "ok"))?, (1, 0));
        assert_eq!(committed.try_recv()?, 1);
        assert!(writer.ingest(batch("missing-source", "rollback")).is_err());
        assert!(matches!(
            committed.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        Ok(())
    }
}
