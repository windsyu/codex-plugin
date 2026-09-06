use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::domain::gateway::{
    GatewayCommandRecord, GatewayTransition, NewGatewayCommand, ReceiveGatewayCommand,
};
use crate::domain::model::OwnedIngestBatch;
use crate::domain::session::{
    AcquireThreadLease, CompleteTurnOwner, PersistInputLease, RegisterSessionWorker,
    SessionWorkerRecord, SessionWorkerRegistration, SessionWorkerTransition, TurnOwnerRecord,
};
use crate::store::{
    ClaimedImageUpload, Database, NewImageUpload, PendingRequestClaim, StageImageUpload,
};

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

#[allow(dead_code)] // Legacy source/image commands remain for migration contract tests.
enum ControlCommand {
    UpsertSource {
        source_id: String,
        kind: String,
        stable_identity: String,
        config: Value,
        status: String,
        reply: mpsc::Sender<Result<()>>,
    },
    OpenSourceEpoch {
        source_id: String,
        source_epoch: String,
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
    OpenWorkerConnection {
        worker_id: String,
        source_id: String,
        source_epoch: String,
        connection_epoch: String,
        reply: mpsc::Sender<Result<()>>,
    },
    CloseWorkerConnection {
        worker_id: String,
        connection_epoch: String,
        last_proxy_seq: u64,
        reason: String,
        reply: mpsc::Sender<Result<()>>,
    },
    RegisterSessionWorker {
        registration: SessionWorkerRegistration,
        reply: mpsc::Sender<Result<RegisterSessionWorker>>,
    },
    UpgradeThreadReservation {
        lease_id: String,
        worker_id: String,
        expected_version: i64,
        codex_thread_id: String,
        command_id: Option<String>,
        reply: mpsc::Sender<Result<AcquireThreadLease>>,
    },
    AcquireThreadLease {
        lease_id: String,
        source_id: String,
        source_epoch: String,
        codex_thread_id: String,
        worker_id: String,
        role: String,
        command_id: Option<String>,
        reply: mpsc::Sender<Result<AcquireThreadLease>>,
    },
    ReserveThreadLease {
        lease_id: String,
        reservation_id: String,
        source_id: String,
        source_epoch: String,
        worker_id: String,
        role: String,
        command_id: Option<String>,
        reply: mpsc::Sender<Result<crate::domain::session::ThreadLeaseRecord>>,
    },
    ReleaseThreadLease {
        source_id: String,
        source_epoch: String,
        codex_thread_id: String,
        worker_id: String,
        command_id: Option<String>,
        reply: mpsc::Sender<Result<crate::domain::session::ThreadLeaseRecord>>,
    },
    CloseThreadReservation {
        lease_id: String,
        worker_id: String,
        to_state: String,
        reason_code: String,
        command_id: Option<String>,
        reply: mpsc::Sender<Result<crate::domain::session::ThreadLeaseRecord>>,
    },
    TransitionSessionWorker {
        transition: SessionWorkerTransition,
        reply: mpsc::Sender<Result<SessionWorkerRecord>>,
    },
    FailSessionBeforeWrite {
        worker_id: String,
        error_code: String,
        command_id: String,
        reply: mpsc::Sender<Result<()>>,
    },
    FinalizeSessionWorker {
        worker_id: String,
        requested_state: String,
        error_code: Option<String>,
        reason_code: String,
        reply: mpsc::Sender<Result<SessionWorkerRecord>>,
    },
    CreateTerminalAttachment {
        attachment_id: String,
        worker_id: String,
        principal_id: String,
        control_token_hash: String,
        reply: mpsc::Sender<Result<()>>,
    },
    TransitionTerminalAttachment {
        attachment_id: String,
        principal_id: String,
        to_state: String,
        reply: mpsc::Sender<Result<()>>,
    },
    AcquireInputLease {
        lease_id: String,
        worker_id: String,
        owner_id: String,
        expected_version: i64,
        reply: mpsc::Sender<Result<PersistInputLease>>,
    },
    ReleaseInputLease {
        lease_id: String,
        worker_id: String,
        owner_id: String,
        expected_version: i64,
        reply: mpsc::Sender<Result<PersistInputLease>>,
    },
    UpsertTurnOwner {
        owner: TurnOwnerRecord,
        reply: mpsc::Sender<Result<TurnOwnerRecord>>,
    },
    CompleteTurnOwner {
        completion: CompleteTurnOwner,
        reply: mpsc::Sender<Result<TurnOwnerRecord>>,
    },
    CompletePendingRequestAction {
        command_id: String,
        request_id: String,
        reply: mpsc::Sender<Result<GatewayCommandRecord>>,
    },
    ReceiveGatewayCommand {
        command: NewGatewayCommand,
        reply: mpsc::Sender<Result<ReceiveGatewayCommand>>,
    },
    TransitionGatewayCommand {
        transition: GatewayTransition,
        reply: mpsc::Sender<Result<GatewayCommandRecord>>,
    },
    ClaimPendingRequest {
        command_id: String,
        reply: mpsc::Sender<Result<PendingRequestClaim>>,
    },
    StageImageUpload {
        upload: NewImageUpload,
        reply: mpsc::Sender<Result<StageImageUpload>>,
    },
    ClaimImageUploads {
        command_id: String,
        principal_id: String,
        upload_ids: Vec<String>,
        reply: mpsc::Sender<Result<Vec<ClaimedImageUpload>>>,
    },
    CleanupCommandImages {
        command_id: String,
        reply: mpsc::Sender<Result<Vec<String>>>,
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

#[allow(dead_code)] // Legacy source/image commands remain for migration contract tests.
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

    pub fn open_source_epoch(&self, source_id: &str, source_epoch: &str) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.control(
            ControlCommand::OpenSourceEpoch {
                source_id: source_id.into(),
                source_epoch: source_epoch.into(),
                reply: tx,
            },
            rx,
        )
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

    pub fn open_worker_connection(
        &self,
        worker_id: &str,
        source_id: &str,
        source_epoch: &str,
        connection_epoch: &str,
    ) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.control(
            ControlCommand::OpenWorkerConnection {
                worker_id: worker_id.into(),
                source_id: source_id.into(),
                source_epoch: source_epoch.into(),
                connection_epoch: connection_epoch.into(),
                reply: tx,
            },
            rx,
        )
    }

    pub fn close_worker_connection(
        &self,
        worker_id: &str,
        connection_epoch: &str,
        last_proxy_seq: u64,
        reason: &str,
    ) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.control(
            ControlCommand::CloseWorkerConnection {
                worker_id: worker_id.into(),
                connection_epoch: connection_epoch.into(),
                last_proxy_seq,
                reason: reason.into(),
                reply: tx,
            },
            rx,
        )
    }

    pub fn register_session_worker(
        &self,
        registration: SessionWorkerRegistration,
    ) -> Result<RegisterSessionWorker> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::RegisterSessionWorker {
                registration,
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn upgrade_thread_reservation(
        &self,
        lease_id: &str,
        worker_id: &str,
        expected_version: i64,
        codex_thread_id: &str,
        command_id: Option<&str>,
    ) -> Result<AcquireThreadLease> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::UpgradeThreadReservation {
                lease_id: lease_id.into(),
                worker_id: worker_id.into(),
                expected_version,
                codex_thread_id: codex_thread_id.into(),
                command_id: command_id.map(str::to_string),
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    #[allow(clippy::too_many_arguments)]
    pub fn acquire_thread_lease(
        &self,
        lease_id: &str,
        source_id: &str,
        source_epoch: &str,
        codex_thread_id: &str,
        worker_id: &str,
        role: &str,
        command_id: Option<&str>,
    ) -> Result<AcquireThreadLease> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::AcquireThreadLease {
                lease_id: lease_id.into(),
                source_id: source_id.into(),
                source_epoch: source_epoch.into(),
                codex_thread_id: codex_thread_id.into(),
                worker_id: worker_id.into(),
                role: role.into(),
                command_id: command_id.map(str::to_string),
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reserve_thread_lease(
        &self,
        lease_id: &str,
        reservation_id: &str,
        source_id: &str,
        source_epoch: &str,
        worker_id: &str,
        role: &str,
        command_id: Option<&str>,
    ) -> Result<crate::domain::session::ThreadLeaseRecord> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::ReserveThreadLease {
                lease_id: lease_id.into(),
                reservation_id: reservation_id.into(),
                source_id: source_id.into(),
                source_epoch: source_epoch.into(),
                worker_id: worker_id.into(),
                role: role.into(),
                command_id: command_id.map(str::to_string),
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn release_thread_lease(
        &self,
        source_id: &str,
        source_epoch: &str,
        codex_thread_id: &str,
        worker_id: &str,
        command_id: Option<&str>,
    ) -> Result<crate::domain::session::ThreadLeaseRecord> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::ReleaseThreadLease {
                source_id: source_id.into(),
                source_epoch: source_epoch.into(),
                codex_thread_id: codex_thread_id.into(),
                worker_id: worker_id.into(),
                command_id: command_id.map(str::to_string),
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn close_thread_reservation(
        &self,
        lease_id: &str,
        worker_id: &str,
        to_state: &str,
        reason_code: &str,
        command_id: Option<&str>,
    ) -> Result<crate::domain::session::ThreadLeaseRecord> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::CloseThreadReservation {
                lease_id: lease_id.into(),
                worker_id: worker_id.into(),
                to_state: to_state.into(),
                reason_code: reason_code.into(),
                command_id: command_id.map(str::to_string),
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn transition_session_worker(
        &self,
        transition: SessionWorkerTransition,
    ) -> Result<SessionWorkerRecord> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::TransitionSessionWorker { transition, reply })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn fail_session_before_write(
        &self,
        worker_id: &str,
        error_code: &str,
        command_id: &str,
    ) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.control(
            ControlCommand::FailSessionBeforeWrite {
                worker_id: worker_id.into(),
                error_code: error_code.into(),
                command_id: command_id.into(),
                reply: tx,
            },
            rx,
        )
    }

    pub fn finalize_session_worker(
        &self,
        worker_id: &str,
        requested_state: &str,
        error_code: Option<&str>,
        reason_code: &str,
    ) -> Result<SessionWorkerRecord> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::FinalizeSessionWorker {
                worker_id: worker_id.into(),
                requested_state: requested_state.into(),
                error_code: error_code.map(str::to_string),
                reason_code: reason_code.into(),
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn create_terminal_attachment(
        &self,
        attachment_id: &str,
        worker_id: &str,
        principal_id: &str,
        control_token_hash: &str,
    ) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.control(
            ControlCommand::CreateTerminalAttachment {
                attachment_id: attachment_id.into(),
                worker_id: worker_id.into(),
                principal_id: principal_id.into(),
                control_token_hash: control_token_hash.into(),
                reply: tx,
            },
            rx,
        )
    }

    pub fn transition_terminal_attachment(
        &self,
        attachment_id: &str,
        principal_id: &str,
        to_state: &str,
    ) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        self.control(
            ControlCommand::TransitionTerminalAttachment {
                attachment_id: attachment_id.into(),
                principal_id: principal_id.into(),
                to_state: to_state.into(),
                reply: tx,
            },
            rx,
        )
    }

    pub fn upsert_turn_owner(&self, owner: TurnOwnerRecord) -> Result<TurnOwnerRecord> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::UpsertTurnOwner { owner, reply })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    #[allow(clippy::too_many_arguments)]
    pub fn complete_turn_owner(
        &self,
        source_id: &str,
        source_epoch: &str,
        codex_thread_id: &str,
        codex_turn_id: &str,
        to_state: &str,
        reason_code: &str,
        command_id: Option<&str>,
    ) -> Result<TurnOwnerRecord> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::CompleteTurnOwner {
                completion: CompleteTurnOwner {
                    source_id: source_id.into(),
                    source_epoch: source_epoch.into(),
                    codex_thread_id: codex_thread_id.into(),
                    codex_turn_id: codex_turn_id.into(),
                    to_state: to_state.into(),
                    reason_code: reason_code.into(),
                    command_id: command_id.map(str::to_string),
                },
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn acquire_input_lease(
        &self,
        lease_id: &str,
        worker_id: &str,
        owner_id: &str,
        expected_version: i64,
    ) -> Result<PersistInputLease> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::AcquireInputLease {
                lease_id: lease_id.into(),
                worker_id: worker_id.into(),
                owner_id: owner_id.into(),
                expected_version,
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn release_input_lease(
        &self,
        lease_id: &str,
        worker_id: &str,
        owner_id: &str,
        expected_version: i64,
    ) -> Result<PersistInputLease> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::ReleaseInputLease {
                lease_id: lease_id.into(),
                worker_id: worker_id.into(),
                owner_id: owner_id.into(),
                expected_version,
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn receive_gateway_command(
        &self,
        command: NewGatewayCommand,
    ) -> Result<ReceiveGatewayCommand> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::ReceiveGatewayCommand { command, reply })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn transition_gateway_command(
        &self,
        transition: GatewayTransition,
    ) -> Result<GatewayCommandRecord> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::TransitionGatewayCommand { transition, reply })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn claim_pending_request(&self, command_id: &str) -> Result<PendingRequestClaim> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::ClaimPendingRequest {
                command_id: command_id.into(),
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn complete_pending_request_action(
        &self,
        command_id: &str,
        request_id: &str,
    ) -> Result<GatewayCommandRecord> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::CompletePendingRequestAction {
                command_id: command_id.into(),
                request_id: request_id.into(),
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn stage_image_upload(&self, upload: NewImageUpload) -> Result<StageImageUpload> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::StageImageUpload { upload, reply })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn claim_image_uploads(
        &self,
        command_id: &str,
        principal_id: &str,
        upload_ids: Vec<String>,
    ) -> Result<Vec<ClaimedImageUpload>> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::ClaimImageUploads {
                command_id: command_id.into(),
                principal_id: principal_id.into(),
                upload_ids,
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
    }

    pub fn cleanup_command_images(&self, command_id: &str) -> Result<Vec<String>> {
        let (reply, response) = mpsc::channel();
        self.control
            .send(ControlCommand::CleanupCommandImages {
                command_id: command_id.into(),
                reply,
            })
            .context("Observer DbWriter is unavailable")?;
        response.recv().context("Observer DbWriter stopped")?
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
    let command = match command {
        ControlCommand::ReceiveGatewayCommand { command, reply } => {
            let _ = reply.send(database.receive_gateway_command_on(connection, &command));
            return false;
        }
        ControlCommand::TransitionGatewayCommand { transition, reply } => {
            let _ = reply.send(database.transition_gateway_command_on(connection, &transition));
            return false;
        }
        ControlCommand::ClaimPendingRequest { command_id, reply } => {
            let _ = reply.send(database.claim_pending_request_on(connection, &command_id));
            return false;
        }
        ControlCommand::RegisterSessionWorker {
            registration,
            reply,
        } => {
            let _ = reply.send(database.register_session_worker_on(connection, &registration));
            return false;
        }
        ControlCommand::UpgradeThreadReservation {
            lease_id,
            worker_id,
            expected_version,
            codex_thread_id,
            command_id,
            reply,
        } => {
            let _ = reply.send(database.upgrade_thread_reservation_on(
                connection,
                &lease_id,
                &worker_id,
                expected_version,
                &codex_thread_id,
                command_id.as_deref(),
            ));
            return false;
        }
        ControlCommand::AcquireThreadLease {
            lease_id,
            source_id,
            source_epoch,
            codex_thread_id,
            worker_id,
            role,
            command_id,
            reply,
        } => {
            let _ = reply.send(database.acquire_thread_lease_on(
                connection,
                &lease_id,
                &source_id,
                &source_epoch,
                &codex_thread_id,
                &worker_id,
                &role,
                command_id.as_deref(),
            ));
            return false;
        }
        ControlCommand::ReserveThreadLease {
            lease_id,
            reservation_id,
            source_id,
            source_epoch,
            worker_id,
            role,
            command_id,
            reply,
        } => {
            let _ = reply.send(database.reserve_thread_lease_on(
                connection,
                &lease_id,
                &reservation_id,
                &source_id,
                &source_epoch,
                &worker_id,
                &role,
                command_id.as_deref(),
            ));
            return false;
        }
        ControlCommand::ReleaseThreadLease {
            source_id,
            source_epoch,
            codex_thread_id,
            worker_id,
            command_id,
            reply,
        } => {
            let _ = reply.send(database.release_thread_lease_on(
                connection,
                &source_id,
                &source_epoch,
                &codex_thread_id,
                &worker_id,
                command_id.as_deref(),
            ));
            return false;
        }
        ControlCommand::CloseThreadReservation {
            lease_id,
            worker_id,
            to_state,
            reason_code,
            command_id,
            reply,
        } => {
            let _ = reply.send(database.close_thread_reservation_on(
                connection,
                &lease_id,
                &worker_id,
                &to_state,
                &reason_code,
                command_id.as_deref(),
            ));
            return false;
        }
        ControlCommand::TransitionSessionWorker { transition, reply } => {
            let _ = reply.send(database.transition_session_worker_on(connection, &transition));
            return false;
        }
        ControlCommand::FailSessionBeforeWrite {
            worker_id,
            error_code,
            command_id,
            reply,
        } => {
            let _ = reply.send(database.fail_session_before_write_on(
                connection,
                &worker_id,
                &error_code,
                &command_id,
            ));
            return false;
        }
        ControlCommand::FinalizeSessionWorker {
            worker_id,
            requested_state,
            error_code,
            reason_code,
            reply,
        } => {
            let _ = reply.send(database.finalize_session_worker_on(
                connection,
                &worker_id,
                &requested_state,
                error_code.as_deref(),
                &reason_code,
            ));
            return false;
        }
        ControlCommand::AcquireInputLease {
            lease_id,
            worker_id,
            owner_id,
            expected_version,
            reply,
        } => {
            let _ = reply.send(database.acquire_input_lease_on(
                connection,
                &lease_id,
                &worker_id,
                &owner_id,
                expected_version,
            ));
            return false;
        }
        ControlCommand::ReleaseInputLease {
            lease_id,
            worker_id,
            owner_id,
            expected_version,
            reply,
        } => {
            let _ = reply.send(database.release_input_lease_on(
                connection,
                &lease_id,
                &worker_id,
                &owner_id,
                expected_version,
            ));
            return false;
        }
        ControlCommand::UpsertTurnOwner { owner, reply } => {
            let _ = reply.send(database.upsert_turn_owner_on(connection, &owner));
            return false;
        }
        ControlCommand::CompleteTurnOwner { completion, reply } => {
            let _ = reply.send(database.complete_turn_owner_on(connection, &completion));
            return false;
        }
        ControlCommand::CompletePendingRequestAction {
            command_id,
            request_id,
            reply,
        } => {
            let _ = reply.send(database.complete_pending_request_action_on(
                connection,
                &command_id,
                &request_id,
            ));
            return false;
        }
        ControlCommand::StageImageUpload { upload, reply } => {
            let _ = reply.send(database.stage_image_upload_on(connection, &upload));
            return false;
        }
        ControlCommand::ClaimImageUploads {
            command_id,
            principal_id,
            upload_ids,
            reply,
        } => {
            let _ = reply.send(database.claim_image_uploads_on(
                connection,
                &command_id,
                &principal_id,
                &upload_ids,
            ));
            return false;
        }
        ControlCommand::CleanupCommandImages { command_id, reply } => {
            let _ = reply.send(database.cleanup_command_images_on(connection, &command_id));
            return false;
        }
        command => command,
    };
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
        ControlCommand::OpenSourceEpoch {
            source_id,
            source_epoch,
            reply,
        } => (
            database.open_source_epoch_on(connection, &source_id, &source_epoch),
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
        ControlCommand::OpenWorkerConnection {
            worker_id,
            source_id,
            source_epoch,
            connection_epoch,
            reply,
        } => (
            database.open_worker_connection_on(
                connection,
                &worker_id,
                &source_id,
                &source_epoch,
                &connection_epoch,
            ),
            reply,
        ),
        ControlCommand::CloseWorkerConnection {
            worker_id,
            connection_epoch,
            last_proxy_seq,
            reason,
            reply,
        } => (
            database.close_worker_connection_on(
                connection,
                &worker_id,
                &connection_epoch,
                last_proxy_seq,
                &reason,
            ),
            reply,
        ),
        ControlCommand::CreateTerminalAttachment {
            attachment_id,
            worker_id,
            principal_id,
            control_token_hash,
            reply,
        } => (
            database.create_terminal_attachment_on(
                connection,
                &attachment_id,
                &worker_id,
                &principal_id,
                &control_token_hash,
            ),
            reply,
        ),
        ControlCommand::TransitionTerminalAttachment {
            attachment_id,
            principal_id,
            to_state,
            reply,
        } => (
            database.transition_terminal_attachment_on(
                connection,
                &attachment_id,
                &principal_id,
                &to_state,
            ),
            reply,
        ),
        ControlCommand::ReceiveGatewayCommand { .. }
        | ControlCommand::TransitionGatewayCommand { .. }
        | ControlCommand::ClaimPendingRequest { .. }
        | ControlCommand::RegisterSessionWorker { .. }
        | ControlCommand::UpgradeThreadReservation { .. }
        | ControlCommand::AcquireThreadLease { .. }
        | ControlCommand::ReserveThreadLease { .. }
        | ControlCommand::ReleaseThreadLease { .. }
        | ControlCommand::CloseThreadReservation { .. }
        | ControlCommand::TransitionSessionWorker { .. }
        | ControlCommand::FailSessionBeforeWrite { .. }
        | ControlCommand::FinalizeSessionWorker { .. }
        | ControlCommand::AcquireInputLease { .. }
        | ControlCommand::ReleaseInputLease { .. }
        | ControlCommand::UpsertTurnOwner { .. }
        | ControlCommand::CompleteTurnOwner { .. }
        | ControlCommand::CompletePendingRequestAction { .. }
        | ControlCommand::StageImageUpload { .. }
        | ControlCommand::ClaimImageUploads { .. }
        | ControlCommand::CleanupCommandImages { .. } => {
            unreachable!("Gateway commands are handled before unit control commands")
        }
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

    fn event(source: &str, id: &str) -> crate::domain::model::NormalizedEvent {
        crate::domain::model::NormalizedEvent {
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
            protocol_direction: None,
            worker_id: None,
            worker_connection_epoch: None,
            proxy_seq: None,
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
        let writer = WriterHandle::start(database.clone(), 100, 16, 16)?;
        writer.upsert_source("source", "fixture", &json!({}), "ready")?;
        let mut committed = writer.subscribe();
        assert_eq!(writer.ingest(batch("source", "ok"))?, (1, 0));
        assert_eq!(committed.try_recv()?, 1);
        for (kind, event_id) in [("disk_full", "rollback-full"), ("io_error", "rollback-io")] {
            database.fail_next_ingest_for_test(kind);
            assert!(writer.ingest(batch("source", event_id)).is_err());
            assert!(matches!(
                committed.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
        }
        assert_eq!(database.max_event_seq()?, 1);
        Ok(())
    }

    #[test]
    fn committed_bus_fans_out_to_one_hundred_consumers() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let writer = WriterHandle::start(database, 100, 16, 512)?;
        writer.upsert_source("source", "fanout-fixture", &json!({}), "ready")?;
        let mut consumers = (0..100).map(|_| writer.subscribe()).collect::<Vec<_>>();
        writer.ingest(batch("source", "fanout"))?;
        for consumer in &mut consumers {
            assert_eq!(consumer.try_recv()?, 1);
        }
        Ok(())
    }

    #[test]
    #[ignore = "explicit 2M-event capacity test; set OBSERVER_RUN_CAPACITY=1"]
    fn two_million_event_capacity_path() -> Result<()> {
        anyhow::ensure!(
            std::env::var("OBSERVER_RUN_CAPACITY").as_deref() == Ok("1"),
            "set OBSERVER_RUN_CAPACITY=1 to run the explicit capacity test"
        );
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let writer = WriterHandle::start(database, 4096, 128, 512)?;
        writer.upsert_source("capacity", "fixture-capacity", &json!({}), "ready")?;
        for batch_index in 0..20_000_u64 {
            let mut ingest = batch("capacity", &format!("capacity-{batch_index}-0"));
            ingest.events = (0..100)
                .map(|offset| {
                    let mut event = event("capacity", &format!("capacity-{batch_index}-{offset}"));
                    event.source_seq = (batch_index * 100 + offset + 1) as i64;
                    event
                })
                .collect();
            ingest.ordinal = (batch_index + 1) * 100;
            writer.ingest(ingest)?;
        }
        Ok(())
    }
}
