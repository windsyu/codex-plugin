use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;
use tokio::sync::{broadcast, oneshot, watch};
use uuid::Uuid;

use crate::domain::gateway::{
    GatewayCommandOrigin, GatewayCommandRecord, GatewayCommandTarget, GatewayTransition,
    NewGatewayCommand, PendingRequestAction, ReceiveGatewayCommand,
};
use crate::domain::identity::thread_key;
use crate::domain::session::{
    InputLeaseRecord, PersistInputLease, RegisterSessionWorker, SessionWorkerRecord,
    SessionWorkerRegistration, SessionWorkerTransition, TerminalAttachmentRecord,
    ThreadLeaseRecord, TurnOwnerRecord,
};
use crate::store::{Database, PendingRequestClaim};
use crate::writer::WriterHandle;

use super::proxy::{
    ProxyConfig, ProxyHandle, ProxyOwner, ProxyOwnerHandle, ProxyServer, SessionProtocolBridge,
    SessionProxyEventSink,
};
use super::pty::{PtyProcess, SpawnSpec, canonical_executable, valid_terminal_size};
use super::runtime_dir::RuntimeRoot;
use super::terminal::{OutputEvent, TerminalJournal, TerminalOutputFilter, TerminalSnapshot};

const COMMAND_QUEUE_CAPACITY: usize = 128;
const OUTPUT_QUEUE_CAPACITY: usize = 64;
const OUTPUT_READ_BYTES: usize = 8 * 1024;
const OUTPUT_JOURNAL_BYTES: usize = 2 * 1024 * 1024;
const OUTPUT_JOURNAL_AGE: Duration = Duration::from_secs(5 * 60);
const ATTACH_DESCRIPTOR_TTL: Duration = Duration::from_secs(30);
const ATTACHMENT_CAPACITY: usize = 64;
const DETACH_GRACE: Duration = Duration::from_secs(5);
const READINESS_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_GRACE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerReadiness {
    OutputMarker,
    AppServerProtocol,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SessionWorkerId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionWorkerState {
    Starting,
    Connecting,
    Ready,
    Detached,
    Stopping,
    Exited,
    StaleEpoch,
    Failed,
}

impl SessionWorkerState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Exited | Self::StaleEpoch | Self::Failed)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessExit {
    pub success: bool,
    pub exit_code: u32,
    pub signal: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputLeaseView {
    pub lease_id: Option<String>,
    pub owner_attachment_id: Option<String>,
    pub version: u64,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerSnapshot {
    pub worker_id: SessionWorkerId,
    pub state: SessionWorkerState,
    pub pid: Option<u32>,
    pub cwd: String,
    pub rows: u16,
    pub cols: u16,
    pub output_seq: u64,
    pub terminal_retained_bytes: usize,
    pub terminal_checkpoint_bytes: usize,
    pub pty_eof: bool,
    pub exit: Option<ProcessExit>,
    pub error_code: Option<String>,
    pub input_lease: InputLeaseView,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    #[serde(flatten)]
    pub snapshot: WorkerSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persisted_worker: Option<SessionWorkerRecord>,
    pub thread_leases: Vec<ThreadLeaseRecord>,
    pub terminal_attachments: Vec<TerminalAttachmentRecord>,
    pub active_turns: Vec<TurnOwnerRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persisted_input_lease: Option<InputLeaseRecord>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDescriptor {
    pub attachment_id: String,
    pub attachment_token: String,
    pub descriptor: String,
    pub descriptor_expires_in_seconds: u64,
    pub input_lease_version: u64,
}

impl std::fmt::Debug for AttachmentDescriptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentDescriptor")
            .field("attachment_id", &self.attachment_id)
            .field("attachment_token", &"[REDACTED]")
            .field("descriptor", &"[REDACTED]")
            .field(
                "descriptor_expires_in_seconds",
                &self.descriptor_expires_in_seconds,
            )
            .field("input_lease_version", &self.input_lease_version)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct CreateFakeSession {
    pub cwd: PathBuf,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone)]
pub struct CreateSession {
    pub source_id: String,
    pub source_epoch: String,
    pub expected_supervisor_version: u64,
    pub mode: String,
    pub codex_thread_id: Option<String>,
    pub cwd: PathBuf,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone)]
pub struct SessionSource {
    pub source_id: String,
    pub store_source_id: String,
    pub codex_home: PathBuf,
    pub upstream_socket: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CreateFakeOutcome {
    pub snapshot: WorkerSnapshot,
    pub replayed: bool,
}

#[derive(Debug, Clone)]
pub struct CreateSessionOutcome {
    pub snapshot: WorkerSnapshot,
    pub replayed: bool,
    pub existing_owner: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpectedActiveTurn {
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone)]
pub struct StopSessionOutcome {
    pub snapshot: WorkerSnapshot,
    pub command: GatewayCommandRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError {
    pub code: &'static str,
    pub message: String,
}

impl SessionError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn busy() -> Self {
        Self::new(
            "SESSION_WORKER_BUSY",
            "Session Worker command queue is full",
        )
    }

    fn unavailable() -> Self {
        Self::new(
            "SESSION_KERNEL_CLI_UNAVAILABLE",
            "the configured fake Session Kernel CLI is unavailable",
        )
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SessionError {}

#[derive(Clone)]
pub struct SessionWorkerHandle {
    inner: Arc<WorkerHandleInner>,
}

struct WorkerHandleInner {
    id: SessionWorkerId,
    commands: SyncSender<WorkerCommand>,
    priority: mpsc::Sender<PriorityCommand>,
    state: watch::Receiver<WorkerSnapshot>,
    output: broadcast::Sender<OutputEvent>,
}

#[derive(Clone)]
struct SessionPersistence {
    writer: WriterHandle,
    worker_id: String,
    proxy_owner: ProxyOwnerHandle,
    initial_input_lease_version: u64,
}

impl std::fmt::Debug for SessionWorkerHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionWorkerHandle")
            .field("id", &self.inner.id)
            .finish_non_exhaustive()
    }
}

impl SessionWorkerHandle {
    fn spawn(
        id: SessionWorkerId,
        spec: SpawnSpec,
        readiness: WorkerReadiness,
        readiness_timeout: Duration,
        runtime_root: RuntimeRoot,
        runtime_dir: PathBuf,
        persistence: Option<SessionPersistence>,
    ) -> Result<Self, SessionError> {
        let cwd = spec.canonical_cwd.to_string_lossy().into_owned();
        let rows = spec.rows;
        let cols = spec.cols;
        let mut process = PtyProcess::spawn(spec).map_err(|error| {
            tracing::error!(
                worker_id = %id.0,
                error = %format!("{error:#}"),
                "failed to spawn Session Worker process"
            );
            let _ = runtime_root.cleanup_worker_dir(&runtime_dir);
            SessionError::new(
                "SESSION_WORKER_SPAWN_FAILED",
                "failed to start Session Worker process",
            )
        })?;
        let pid = process.process_id();
        let reader = process.take_reader().map_err(|error| {
            tracing::error!(
                worker_id = %id.0,
                error = %format!("{error:#}"),
                "failed to initialize Session Worker PTY"
            );
            let _ = process.force_kill();
            let _ = runtime_root.cleanup_worker_dir(&runtime_dir);
            SessionError::new(
                "SESSION_WORKER_SPAWN_FAILED",
                "failed to initialize Session Worker PTY",
            )
        })?;
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        let (priority_tx, priority_rx) = mpsc::channel();
        let (reader_tx, reader_rx) = mpsc::sync_channel(OUTPUT_QUEUE_CAPACITY);
        let (output_tx, _) = broadcast::channel(256);
        let initial_input_lease_version = persistence
            .as_ref()
            .map_or(0, |persistence| persistence.initial_input_lease_version);
        let initial = WorkerSnapshot {
            worker_id: id.clone(),
            state: SessionWorkerState::Starting,
            pid,
            cwd,
            rows,
            cols,
            output_seq: 0,
            terminal_retained_bytes: 0,
            terminal_checkpoint_bytes: 0,
            pty_eof: false,
            exit: None,
            error_code: None,
            input_lease: InputLeaseView {
                lease_id: None,
                owner_attachment_id: None,
                version: initial_input_lease_version,
                state: "none".into(),
            },
        };
        let (state_tx, state_rx) = watch::channel(initial.clone());
        let actor_output = output_tx.clone();
        let failed_actor_runtime_root = runtime_root.clone();
        let failed_actor_runtime_dir = runtime_dir.clone();
        thread::Builder::new()
            .name(format!("session-worker-{}", id.0))
            .spawn(move || {
                WorkerActor::new(
                    process,
                    command_rx,
                    priority_rx,
                    reader_rx,
                    actor_output,
                    state_tx,
                    initial,
                    readiness,
                    readiness_timeout,
                    runtime_root,
                    runtime_dir,
                    persistence,
                )
                .run();
            })
            .map_err(|error| {
                tracing::error!(
                    worker_id = %id.0,
                    error = %error,
                    "failed to start Session Worker actor thread"
                );
                let _ = failed_actor_runtime_root.cleanup_worker_dir(&failed_actor_runtime_dir);
                SessionError::new(
                    "SESSION_WORKER_SPAWN_FAILED",
                    "failed to start Session Worker actor",
                )
            })?;
        thread::Builder::new()
            .name(format!("session-pty-reader-{}", id.0))
            .spawn(move || read_pty(reader, reader_tx))
            .map_err(|error| {
                tracing::error!(
                    worker_id = %id.0,
                    error = %error,
                    "failed to start Session Worker PTY reader thread"
                );
                let _ = priority_tx.send(PriorityCommand::Stop { reply: None });
                SessionError::new(
                    "SESSION_WORKER_SPAWN_FAILED",
                    "failed to start Session Worker PTY reader",
                )
            })?;
        Ok(Self {
            inner: Arc::new(WorkerHandleInner {
                id,
                commands: command_tx,
                priority: priority_tx,
                state: state_rx,
                output: output_tx,
            }),
        })
    }

    #[cfg(test)]
    pub fn id(&self) -> &SessionWorkerId {
        &self.inner.id
    }

    pub fn snapshot(&self) -> WorkerSnapshot {
        self.inner.state.borrow().clone()
    }

    pub fn subscribe_state(&self) -> watch::Receiver<WorkerSnapshot> {
        self.inner.state.clone()
    }

    pub fn subscribe_output(&self) -> broadcast::Receiver<OutputEvent> {
        self.inner.output.subscribe()
    }

    pub async fn prepare_attachment(
        &self,
        principal: String,
        resume_attachment_id: Option<String>,
        resume_attachment_token: Option<String>,
    ) -> Result<AttachmentDescriptor, SessionError> {
        let (reply, receiver) = oneshot::channel();
        self.send(WorkerCommand::PrepareAttachment {
            principal,
            resume_attachment_id,
            resume_attachment_token,
            reply,
        })?;
        receive_reply(receiver).await
    }

    pub async fn connect_attachment(
        &self,
        principal: String,
        descriptor: String,
    ) -> Result<String, SessionError> {
        let (reply, receiver) = oneshot::channel();
        self.send(WorkerCommand::ConnectAttachment {
            principal,
            descriptor,
            reply,
        })?;
        receive_reply(receiver).await
    }

    pub async fn disconnect_attachment(&self, attachment_id: String) {
        let _ = self.send(WorkerCommand::DisconnectAttachment { attachment_id });
    }

    pub async fn acquire_input_lease(
        &self,
        principal: String,
        attachment_id: String,
        attachment_token: String,
        expected_version: u64,
        takeover: bool,
    ) -> Result<InputLeaseView, SessionError> {
        let (reply, receiver) = oneshot::channel();
        self.send(WorkerCommand::AcquireInputLease {
            principal,
            attachment_id,
            attachment_token,
            expected_version,
            takeover,
            reply,
        })?;
        receive_reply(receiver).await
    }

    pub async fn release_input_lease(
        &self,
        principal: String,
        attachment_id: String,
        attachment_token: String,
        lease_id: String,
        expected_version: u64,
    ) -> Result<InputLeaseView, SessionError> {
        let (reply, receiver) = oneshot::channel();
        self.send(WorkerCommand::ReleaseInputLease {
            principal,
            attachment_id,
            attachment_token,
            lease_id,
            expected_version,
            reply,
        })?;
        receive_reply(receiver).await
    }

    pub async fn write_input(
        &self,
        attachment_id: String,
        lease_id: String,
        data: Vec<u8>,
    ) -> Result<(), SessionError> {
        let (reply, receiver) = oneshot::channel();
        self.send(WorkerCommand::Input {
            attachment_id,
            lease_id,
            data,
            reply,
        })?;
        receive_reply(receiver).await
    }

    pub async fn resize(
        &self,
        attachment_id: String,
        rows: u16,
        cols: u16,
    ) -> Result<WorkerSnapshot, SessionError> {
        let (reply, receiver) = oneshot::channel();
        self.send(WorkerCommand::Resize {
            attachment_id,
            rows,
            cols,
            reply,
        })?;
        receive_reply(receiver).await
    }

    pub async fn acknowledge(&self, attachment_id: String, output_seq: u64) {
        let _ = self.send(WorkerCommand::Acknowledge {
            attachment_id,
            output_seq,
        });
    }

    pub async fn terminal_snapshot(
        &self,
        after_seq: Option<u64>,
    ) -> Result<TerminalSnapshot, SessionError> {
        let (reply, receiver) = oneshot::channel();
        self.send(WorkerCommand::Snapshot { after_seq, reply })?;
        receive_reply(receiver).await
    }

    pub async fn stop(&self) -> Result<WorkerSnapshot, SessionError> {
        let (reply, receiver) = oneshot::channel();
        self.inner
            .priority
            .send(PriorityCommand::Stop { reply: Some(reply) })
            .map_err(|_| SessionError::new("SESSION_WORKER_EXITED", "Session Worker has exited"))?;
        receive_reply(receiver).await
    }

    pub(super) fn protocol_ready(&self) -> Result<(), SessionError> {
        self.send(WorkerCommand::ProtocolReady)
    }

    pub(super) fn protocol_connected(&self) -> Result<(), SessionError> {
        self.send(WorkerCommand::ProtocolConnected)
    }

    pub(super) fn protocol_failed(&self, error_code: &'static str) -> Result<(), SessionError> {
        self.send(WorkerCommand::ProtocolFailed { error_code })
    }

    fn source_stale(&self) -> Result<(), SessionError> {
        self.send(WorkerCommand::SourceStale)
    }

    fn send(&self, command: WorkerCommand) -> Result<(), SessionError> {
        match self.inner.commands.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(SessionError::busy()),
            Err(TrySendError::Disconnected(_)) => Err(SessionError::new(
                "SESSION_WORKER_EXITED",
                "Session Worker has exited",
            )),
        }
    }
}

async fn receive_reply<T>(
    receiver: oneshot::Receiver<Result<T, SessionError>>,
) -> Result<T, SessionError> {
    tokio::time::timeout(Duration::from_secs(3), receiver)
        .await
        .map_err(|_| SessionError::new("SESSION_WORKER_BUSY", "Session Worker did not respond"))?
        .map_err(|_| SessionError::new("SESSION_WORKER_EXITED", "Session Worker has exited"))?
}

enum WorkerCommand {
    ProtocolConnected,
    ProtocolReady,
    SourceStale,
    ProtocolFailed {
        error_code: &'static str,
    },
    PrepareAttachment {
        principal: String,
        resume_attachment_id: Option<String>,
        resume_attachment_token: Option<String>,
        reply: oneshot::Sender<Result<AttachmentDescriptor, SessionError>>,
    },
    ConnectAttachment {
        principal: String,
        descriptor: String,
        reply: oneshot::Sender<Result<String, SessionError>>,
    },
    DisconnectAttachment {
        attachment_id: String,
    },
    AcquireInputLease {
        principal: String,
        attachment_id: String,
        attachment_token: String,
        expected_version: u64,
        takeover: bool,
        reply: oneshot::Sender<Result<InputLeaseView, SessionError>>,
    },
    ReleaseInputLease {
        principal: String,
        attachment_id: String,
        attachment_token: String,
        lease_id: String,
        expected_version: u64,
        reply: oneshot::Sender<Result<InputLeaseView, SessionError>>,
    },
    Input {
        attachment_id: String,
        lease_id: String,
        data: Vec<u8>,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    Resize {
        attachment_id: String,
        rows: u16,
        cols: u16,
        reply: oneshot::Sender<Result<WorkerSnapshot, SessionError>>,
    },
    Acknowledge {
        attachment_id: String,
        output_seq: u64,
    },
    Snapshot {
        after_seq: Option<u64>,
        reply: oneshot::Sender<Result<TerminalSnapshot, SessionError>>,
    },
}

enum PriorityCommand {
    Stop {
        reply: Option<oneshot::Sender<Result<WorkerSnapshot, SessionError>>>,
    },
}

enum ReaderEvent {
    Output(Vec<u8>),
    Eof,
    Failed,
}

struct Attachment {
    principal: String,
    control_token_hash: blake3::Hash,
    descriptor_hash: Option<blake3::Hash>,
    descriptor_expires_at: Instant,
    connected: bool,
    disconnected_at: Option<Instant>,
    last_ack: u64,
}

#[derive(Default)]
struct InputLease {
    lease_id: Option<String>,
    attachment_id: Option<String>,
    version: u64,
}

impl InputLease {
    fn view(&self) -> InputLeaseView {
        InputLeaseView {
            lease_id: self.lease_id.clone(),
            owner_attachment_id: self.attachment_id.clone(),
            version: self.version,
            state: if self.lease_id.is_some() {
                "active".into()
            } else {
                "none".into()
            },
        }
    }

    fn release_to_version(&mut self, version: u64) {
        self.lease_id = None;
        self.attachment_id = None;
        self.version = version;
    }
}

struct WorkerActor {
    process: PtyProcess,
    commands: Receiver<WorkerCommand>,
    priority: Receiver<PriorityCommand>,
    reader: Receiver<ReaderEvent>,
    output: broadcast::Sender<OutputEvent>,
    state_sender: watch::Sender<WorkerSnapshot>,
    state: WorkerSnapshot,
    journal: TerminalJournal,
    terminal_filter: TerminalOutputFilter,
    attachments: HashMap<String, Attachment>,
    input_lease: InputLease,
    readiness_window: Vec<u8>,
    readiness_deadline: Option<Instant>,
    readiness: WorkerReadiness,
    stop_deadline: Option<Instant>,
    child_exit: Option<ProcessExit>,
    pty_eof: bool,
    runtime_root: RuntimeRoot,
    runtime_dir: PathBuf,
    persistence: Option<SessionPersistence>,
}

impl WorkerActor {
    #[allow(clippy::too_many_arguments)]
    fn new(
        process: PtyProcess,
        commands: Receiver<WorkerCommand>,
        priority: Receiver<PriorityCommand>,
        reader: Receiver<ReaderEvent>,
        output: broadcast::Sender<OutputEvent>,
        state_sender: watch::Sender<WorkerSnapshot>,
        state: WorkerSnapshot,
        readiness: WorkerReadiness,
        readiness_timeout: Duration,
        runtime_root: RuntimeRoot,
        runtime_dir: PathBuf,
        persistence: Option<SessionPersistence>,
    ) -> Self {
        let journal = TerminalJournal::new(
            state.rows,
            state.cols,
            OUTPUT_JOURNAL_BYTES,
            OUTPUT_JOURNAL_AGE,
        );
        let input_lease_version = state.input_lease.version;
        Self {
            process,
            commands,
            priority,
            reader,
            output,
            state_sender,
            state,
            journal,
            terminal_filter: TerminalOutputFilter::default(),
            attachments: HashMap::new(),
            input_lease: InputLease {
                version: input_lease_version,
                ..InputLease::default()
            },
            readiness_window: Vec::new(),
            readiness_deadline: Some(Instant::now() + readiness_timeout),
            readiness,
            stop_deadline: None,
            child_exit: None,
            pty_eof: false,
            runtime_root,
            runtime_dir,
            persistence,
        }
    }

    fn run(mut self) {
        self.transition(SessionWorkerState::Connecting, None);
        loop {
            while let Ok(command) = self.priority.try_recv() {
                self.handle_priority(command);
            }
            for _ in 0..32 {
                match self.commands.try_recv() {
                    Ok(command) => self.handle_command(command),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }
            for _ in 0..32 {
                match self.reader.try_recv() {
                    Ok(event) => self.handle_reader(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.pty_eof = true;
                        break;
                    }
                }
            }
            self.expire_detached_lease();
            self.cleanup_attachments();
            self.enforce_readiness_timeout();
            self.poll_child();
            if self
                .stop_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
                && self.child_exit.is_none()
            {
                let _ = self.process.force_kill();
                self.stop_deadline = None;
            }
            if self.child_exit.is_some() && self.pty_eof {
                self.state.exit = self.child_exit.clone();
                if !self.state.state.is_terminal() {
                    self.state.error_code = if self.state.state == SessionWorkerState::Stopping {
                        None
                    } else if self.state.state == SessionWorkerState::Connecting {
                        Some("SESSION_WORKER_NOT_READY".into())
                    } else {
                        self.state
                            .exit
                            .as_ref()
                            .filter(|exit| !exit.success)
                            .map(|_| "SESSION_WORKER_EXITED".into())
                    };
                    self.transition(SessionWorkerState::Exited, None);
                } else {
                    self.publish();
                }
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        if let Some(persistence) = &self.persistence {
            persistence.proxy_owner.set(None);
            let requested_state = match self.state.state {
                SessionWorkerState::Failed => "failed",
                SessionWorkerState::StaleEpoch => "stale_epoch",
                _ => "exited",
            };
            let reason_code = self
                .state
                .error_code
                .as_deref()
                .unwrap_or("worker_process_exited");
            if let Err(error) = persistence.writer.finalize_session_worker(
                &persistence.worker_id,
                requested_state,
                self.state.error_code.as_deref(),
                reason_code,
            ) {
                tracing::error!(
                    worker_id = %persistence.worker_id,
                    error = %format!("{error:#}"),
                    "failed to persist final Session Worker state"
                );
            }
        }
        let _ = self.runtime_root.cleanup_worker_dir(&self.runtime_dir);
    }

    fn handle_priority(&mut self, command: PriorityCommand) {
        match command {
            PriorityCommand::Stop { reply } => {
                if self.state.state.is_terminal() {
                    if let Some(reply) = reply {
                        let _ = reply.send(Ok(self.state.clone()));
                    }
                    return;
                }
                if self.state.state != SessionWorkerState::Stopping {
                    self.readiness_deadline = None;
                    self.transition(SessionWorkerState::Stopping, None);
                    if let Err(error) = self.process.terminate() {
                        self.state.error_code = Some("SESSION_WORKER_STOP_FAILED".into());
                        tracing::warn!(error = %error, worker_id = %self.state.worker_id.0, "failed to terminate Session Worker gracefully");
                    }
                    self.stop_deadline = Some(Instant::now() + STOP_GRACE);
                }
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(self.state.clone()));
                }
            }
        }
    }

    fn handle_command(&mut self, command: WorkerCommand) {
        match command {
            WorkerCommand::ProtocolConnected => {
                if self.readiness == WorkerReadiness::AppServerProtocol
                    && self.state.state == SessionWorkerState::Connecting
                {
                    self.readiness_deadline = None;
                }
            }
            WorkerCommand::ProtocolReady => {
                if self.readiness == WorkerReadiness::AppServerProtocol
                    && self.state.state == SessionWorkerState::Connecting
                {
                    self.readiness_deadline = None;
                    self.transition(SessionWorkerState::Ready, None);
                }
            }
            WorkerCommand::SourceStale => {
                if !self.state.state.is_terminal()
                    && self.state.state != SessionWorkerState::Stopping
                {
                    self.readiness_deadline = None;
                    self.transition(SessionWorkerState::StaleEpoch, Some("SOURCE_EPOCH_STALE"));
                    let _ = self.process.terminate();
                    self.stop_deadline = Some(Instant::now() + STOP_GRACE);
                }
            }
            WorkerCommand::ProtocolFailed { error_code } => {
                if !self.state.state.is_terminal()
                    && self.state.state != SessionWorkerState::Stopping
                {
                    self.readiness_deadline = None;
                    self.transition(SessionWorkerState::Failed, Some(error_code));
                    let _ = self.process.terminate();
                    self.stop_deadline = Some(Instant::now() + STOP_GRACE);
                }
            }
            WorkerCommand::PrepareAttachment {
                principal,
                resume_attachment_id,
                resume_attachment_token,
                reply,
            } => {
                let _ = reply.send(self.prepare_attachment(
                    principal,
                    resume_attachment_id,
                    resume_attachment_token,
                ));
            }
            WorkerCommand::ConnectAttachment {
                principal,
                descriptor,
                reply,
            } => {
                let _ = reply.send(self.connect_attachment(&principal, &descriptor));
            }
            WorkerCommand::DisconnectAttachment { attachment_id } => {
                if let Some(attachment) = self.attachments.get_mut(&attachment_id) {
                    if let Some(persistence) = &self.persistence
                        && let Err(error) = persistence.writer.transition_terminal_attachment(
                            &attachment_id,
                            &attachment.principal,
                            "detached",
                        )
                    {
                        tracing::warn!(
                            worker_id = %self.state.worker_id.0,
                            attachment_id,
                            error = %format!("{error:#}"),
                            "failed to persist terminal attachment detach"
                        );
                    }
                    attachment.connected = false;
                    attachment.disconnected_at = Some(Instant::now());
                }
                if !self.state.state.is_terminal()
                    && self.state.state != SessionWorkerState::Stopping
                    && !self
                        .attachments
                        .values()
                        .any(|attachment| attachment.connected)
                {
                    self.transition(SessionWorkerState::Detached, None);
                }
            }
            WorkerCommand::AcquireInputLease {
                principal,
                attachment_id,
                attachment_token,
                expected_version,
                takeover,
                reply,
            } => {
                let _ = reply.send(self.acquire_input_lease(
                    &principal,
                    &attachment_id,
                    &attachment_token,
                    expected_version,
                    takeover,
                ));
            }
            WorkerCommand::ReleaseInputLease {
                principal,
                attachment_id,
                attachment_token,
                lease_id,
                expected_version,
                reply,
            } => {
                let _ = reply.send(self.release_input_lease(
                    &principal,
                    &attachment_id,
                    &attachment_token,
                    &lease_id,
                    expected_version,
                ));
            }
            WorkerCommand::Input {
                attachment_id,
                lease_id,
                data,
                reply,
            } => {
                let result = self
                    .require_input_owner(&attachment_id, &lease_id)
                    .and_then(|()| {
                        if data.len() > 64 * 1024 {
                            return Err(SessionError::new(
                                "TERMINAL_FRAME_TOO_LARGE",
                                "terminal input exceeds 64 KiB",
                            ));
                        }
                        self.process.write_input(&data).map_err(|_| {
                            SessionError::new(
                                "SESSION_WORKER_EXITED",
                                "failed to write Session Worker PTY input",
                            )
                        })
                    });
                let _ = reply.send(result);
            }
            WorkerCommand::Resize {
                attachment_id,
                rows,
                cols,
                reply,
            } => {
                let result = self.resize(&attachment_id, rows, cols);
                let _ = reply.send(result);
            }
            WorkerCommand::Acknowledge {
                attachment_id,
                output_seq,
            } => {
                if output_seq <= self.journal.last_seq()
                    && let Some(attachment) = self.attachments.get_mut(&attachment_id)
                {
                    attachment.last_ack = attachment.last_ack.max(output_seq);
                }
            }
            WorkerCommand::Snapshot { after_seq, reply } => {
                let _ = reply.send(Ok(self.journal.snapshot(after_seq)));
            }
        }
    }

    fn prepare_attachment(
        &mut self,
        principal: String,
        resume_attachment_id: Option<String>,
        resume_attachment_token: Option<String>,
    ) -> Result<AttachmentDescriptor, SessionError> {
        if self.state.state.is_terminal() || self.state.state == SessionWorkerState::Stopping {
            return Err(SessionError::new(
                "SESSION_WORKER_EXITED",
                "Session Worker is not attachable",
            ));
        }
        let (attachment_id, attachment_token) =
            match (resume_attachment_id, resume_attachment_token) {
                (Some(attachment_id), Some(attachment_token)) => {
                    let attachment = self.attachments.get(&attachment_id).ok_or_else(|| {
                        SessionError::new(
                            "ATTACHMENT_NOT_FOUND",
                            "terminal attachment was not found",
                        )
                    })?;
                    if attachment.principal != principal {
                        return Err(SessionError::new(
                            "ATTACHMENT_PRINCIPAL_MISMATCH",
                            "terminal attachment belongs to another principal",
                        ));
                    }
                    if !credential_hash_matches(&attachment.control_token_hash, &attachment_token) {
                        return Err(SessionError::new(
                            "ATTACHMENT_TOKEN_INVALID",
                            "terminal attachment control token is invalid",
                        ));
                    }
                    if attachment.connected {
                        return Err(SessionError::new(
                            "ATTACHMENT_ALREADY_CONNECTED",
                            "terminal attachment already has a live connection",
                        ));
                    }
                    if attachment
                        .disconnected_at
                        .is_some_and(|at| at.elapsed() > DETACH_GRACE)
                    {
                        return Err(SessionError::new(
                            "ATTACHMENT_EXPIRED",
                            "terminal attachment reconnect grace expired",
                        ));
                    }
                    (attachment_id, attachment_token)
                }
                (None, None) => {
                    self.cleanup_attachments();
                    if self.attachments.len() >= ATTACHMENT_CAPACITY {
                        return Err(SessionError::new(
                            "ATTACHMENT_LIMIT_REACHED",
                            "Session Worker attachment limit was reached",
                        ));
                    }
                    let attachment_id = Uuid::new_v4().to_string();
                    let attachment_token = opaque_credential();
                    let control_token_hash = blake3::hash(attachment_token.as_bytes());
                    if let Some(persistence) = &self.persistence {
                        persistence
                            .writer
                            .create_terminal_attachment(
                                &attachment_id,
                                &persistence.worker_id,
                                &principal,
                                control_token_hash.to_hex().as_ref(),
                            )
                            .map_err(session_store_error)?;
                    }
                    self.attachments.insert(
                        attachment_id.clone(),
                        Attachment {
                            principal: principal.clone(),
                            control_token_hash,
                            descriptor_hash: None,
                            descriptor_expires_at: Instant::now(),
                            connected: false,
                            disconnected_at: None,
                            last_ack: 0,
                        },
                    );
                    (attachment_id, attachment_token)
                }
                _ => {
                    return Err(SessionError::new(
                        "ATTACHMENT_TOKEN_REQUIRED",
                        "resume attachment ID and control token must be supplied together",
                    ));
                }
            };
        let descriptor = opaque_credential();
        let attachment = self
            .attachments
            .get_mut(&attachment_id)
            .expect("attachment was inserted or resolved");
        attachment.descriptor_hash = Some(blake3::hash(descriptor.as_bytes()));
        attachment.descriptor_expires_at = Instant::now() + ATTACH_DESCRIPTOR_TTL;
        Ok(AttachmentDescriptor {
            attachment_id,
            attachment_token,
            descriptor,
            descriptor_expires_in_seconds: ATTACH_DESCRIPTOR_TTL.as_secs(),
            input_lease_version: self.input_lease.version,
        })
    }

    fn connect_attachment(
        &mut self,
        principal: &str,
        descriptor: &str,
    ) -> Result<String, SessionError> {
        let hash = blake3::hash(descriptor.as_bytes());
        let Some((attachment_id, attachment)) = self
            .attachments
            .iter()
            .find(|(_, attachment)| attachment.descriptor_hash.as_ref() == Some(&hash))
        else {
            return Err(SessionError::new(
                "ATTACHMENT_DESCRIPTOR_INVALID",
                "terminal attachment descriptor is invalid",
            ));
        };
        if attachment.principal != principal {
            return Err(SessionError::new(
                "ATTACHMENT_PRINCIPAL_MISMATCH",
                "terminal attachment belongs to another principal",
            ));
        }
        if attachment.descriptor_expires_at <= Instant::now() {
            let attachment_id = attachment_id.clone();
            if let Some(persistence) = &self.persistence {
                let _ = persistence.writer.transition_terminal_attachment(
                    &attachment_id,
                    principal,
                    "expired",
                );
            }
            self.attachments.remove(&attachment_id);
            return Err(SessionError::new(
                "ATTACHMENT_DESCRIPTOR_EXPIRED",
                "terminal attachment descriptor expired",
            ));
        }
        if attachment.connected {
            return Err(SessionError::new(
                "ATTACHMENT_ALREADY_CONNECTED",
                "terminal attachment already has a live connection",
            ));
        }
        let attachment_id = attachment_id.clone();
        if let Some(persistence) = &self.persistence {
            persistence
                .writer
                .transition_terminal_attachment(&attachment_id, principal, "connected")
                .map_err(session_store_error)?;
        }
        let attachment = self
            .attachments
            .get_mut(&attachment_id)
            .expect("attachment was resolved");
        attachment.descriptor_hash = None;
        attachment.connected = true;
        attachment.disconnected_at = None;
        if matches!(self.state.state, SessionWorkerState::Detached) {
            self.transition(SessionWorkerState::Ready, None);
        }
        Ok(attachment_id)
    }

    fn acquire_input_lease(
        &mut self,
        principal: &str,
        attachment_id: &str,
        attachment_token: &str,
        expected_version: u64,
        takeover: bool,
    ) -> Result<InputLeaseView, SessionError> {
        let attachment =
            self.require_attachment_control(principal, attachment_id, attachment_token)?;
        if !attachment.connected {
            return Err(SessionError::new(
                "ATTACHMENT_NOT_CONNECTED",
                "terminal attachment must connect before acquiring input",
            ));
        }
        if expected_version != self.input_lease.version {
            return Err(SessionError::new(
                "INPUT_LEASE_CONFLICT",
                "input lease version is stale",
            ));
        }
        if self.input_lease.attachment_id.as_deref() == Some(attachment_id) {
            return Ok(self.input_lease.view());
        }
        if self.input_lease.attachment_id.is_some() && !takeover {
            return Err(SessionError::new(
                "INPUT_LEASE_CONFLICT",
                "another attachment owns terminal input",
            ));
        }
        if self.input_lease.attachment_id.is_some() {
            return Err(SessionError::new(
                "INPUT_LEASE_CONFLICT",
                "active terminal input cannot be taken over",
            ));
        }
        let lease_id = Uuid::new_v4().to_string();
        let next_version = if let Some(persistence) = &self.persistence {
            match persistence
                .writer
                .acquire_input_lease(
                    &lease_id,
                    &persistence.worker_id,
                    attachment_id,
                    i64::try_from(expected_version).map_err(|_| {
                        SessionError::new("INPUT_LEASE_CONFLICT", "input lease version overflow")
                    })?,
                )
                .map_err(session_store_error)?
            {
                PersistInputLease::Acquired {
                    lease_id: persisted_lease_id,
                    version,
                } if persisted_lease_id == lease_id => u64::try_from(version).map_err(|_| {
                    SessionError::new("INPUT_LEASE_CONFLICT", "invalid persisted lease version")
                })?,
                PersistInputLease::Acquired { .. } | PersistInputLease::Conflict { .. } => {
                    return Err(SessionError::new(
                        "INPUT_LEASE_CONFLICT",
                        "input lease version or owner is stale",
                    ));
                }
                PersistInputLease::Released { .. } => {
                    return Err(SessionError::new(
                        "SESSION_PERSISTENCE_FAILED",
                        "unexpected input lease persistence outcome",
                    ));
                }
            }
        } else {
            self.input_lease.version.saturating_add(1)
        };
        self.input_lease.version = next_version;
        self.input_lease.attachment_id = Some(attachment_id.into());
        self.input_lease.lease_id = Some(lease_id.clone());
        if let Some(persistence) = &self.persistence {
            persistence.proxy_owner.set(Some(ProxyOwner {
                owner_type: "terminal".into(),
                owner_id: attachment_id.into(),
                principal_id: principal.into(),
                input_lease_id: Some(lease_id),
            }));
        }
        self.publish();
        Ok(self.input_lease.view())
    }

    fn release_input_lease(
        &mut self,
        principal: &str,
        attachment_id: &str,
        attachment_token: &str,
        lease_id: &str,
        expected_version: u64,
    ) -> Result<InputLeaseView, SessionError> {
        self.require_attachment_control(principal, attachment_id, attachment_token)?;
        if expected_version != self.input_lease.version
            || self.input_lease.attachment_id.as_deref() != Some(attachment_id)
            || self.input_lease.lease_id.as_deref() != Some(lease_id)
        {
            return Err(SessionError::new(
                "INPUT_LEASE_CONFLICT",
                "input lease owner or version is stale",
            ));
        }
        let next_version = if let Some(persistence) = &self.persistence {
            match persistence
                .writer
                .release_input_lease(
                    lease_id,
                    &persistence.worker_id,
                    attachment_id,
                    i64::try_from(expected_version).map_err(|_| {
                        SessionError::new("INPUT_LEASE_CONFLICT", "input lease version overflow")
                    })?,
                )
                .map_err(session_store_error)?
            {
                PersistInputLease::Released { version } => {
                    u64::try_from(version).map_err(|_| {
                        SessionError::new("INPUT_LEASE_CONFLICT", "invalid persisted lease version")
                    })?
                }
                PersistInputLease::Acquired { .. } | PersistInputLease::Conflict { .. } => {
                    return Err(SessionError::new(
                        "INPUT_LEASE_CONFLICT",
                        "input lease owner or version is stale",
                    ));
                }
            }
        } else {
            self.input_lease.version.saturating_add(1)
        };
        self.input_lease.release_to_version(next_version);
        if let Some(persistence) = &self.persistence {
            persistence.proxy_owner.set(None);
        }
        self.publish();
        Ok(self.input_lease.view())
    }

    fn require_attachment_control(
        &self,
        principal: &str,
        attachment_id: &str,
        attachment_token: &str,
    ) -> Result<&Attachment, SessionError> {
        let attachment = self.attachments.get(attachment_id).ok_or_else(|| {
            SessionError::new("ATTACHMENT_NOT_FOUND", "terminal attachment was not found")
        })?;
        if attachment.principal != principal {
            return Err(SessionError::new(
                "ATTACHMENT_PRINCIPAL_MISMATCH",
                "terminal attachment belongs to another principal",
            ));
        }
        if !credential_hash_matches(&attachment.control_token_hash, attachment_token) {
            return Err(SessionError::new(
                "ATTACHMENT_TOKEN_INVALID",
                "terminal attachment control token is invalid",
            ));
        }
        Ok(attachment)
    }

    fn require_input_owner(&self, attachment_id: &str, lease_id: &str) -> Result<(), SessionError> {
        if self.state.state == SessionWorkerState::Stopping || self.state.state.is_terminal() {
            return Err(SessionError::new(
                "SESSION_WORKER_EXITED",
                "Session Worker no longer accepts input",
            ));
        }
        if self.input_lease.attachment_id.as_deref() != Some(attachment_id)
            || self.input_lease.lease_id.as_deref() != Some(lease_id)
        {
            return Err(SessionError::new(
                "INPUT_LEASE_REQUIRED",
                "terminal input requires the active input lease",
            ));
        }
        Ok(())
    }

    fn resize(
        &mut self,
        attachment_id: &str,
        rows: u16,
        cols: u16,
    ) -> Result<WorkerSnapshot, SessionError> {
        if self.input_lease.attachment_id.as_deref() != Some(attachment_id) {
            return Err(SessionError::new(
                "INPUT_LEASE_REQUIRED",
                "only the input owner can resize the PTY",
            ));
        }
        if !valid_terminal_size(rows, cols) {
            return Err(SessionError::new(
                "TERMINAL_SIZE_INVALID",
                "terminal dimensions are outside supported bounds",
            ));
        }
        if self.state.rows == rows && self.state.cols == cols {
            return Ok(self.state.clone());
        }
        self.process.resize(rows, cols).map_err(|_| {
            SessionError::new(
                "SESSION_WORKER_EXITED",
                "failed to resize Session Worker PTY",
            )
        })?;
        self.state.rows = rows;
        self.state.cols = cols;
        self.journal.resize(rows, cols);
        self.publish();
        Ok(self.state.clone())
    }

    fn handle_reader(&mut self, event: ReaderEvent) {
        match event {
            ReaderEvent::Output(data) => {
                let filtered = self.terminal_filter.process(&data);
                if !filtered.display_bytes.is_empty() {
                    if self.readiness == WorkerReadiness::OutputMarker
                        && self.state.state == SessionWorkerState::Connecting
                    {
                        self.readiness_window
                            .extend_from_slice(&filtered.display_bytes);
                        if self.readiness_window.len() > 512 {
                            let drain = self.readiness_window.len() - 512;
                            self.readiness_window.drain(..drain);
                        }
                        if contains_bytes(&self.readiness_window, b"SESSION_READY") {
                            self.readiness_deadline = None;
                            self.transition(SessionWorkerState::Ready, None);
                        }
                    }
                    let event = self.journal.append(filtered.display_bytes);
                    self.state.output_seq = event.output_seq;
                    self.publish();
                    let _ = self.output.send(event);
                }
                let cursor_position = self.journal.cursor_position();
                for request in filtered.terminal_replies {
                    let reply = request.encode(cursor_position);
                    if let Err(error) = self.process.write_input(&reply) {
                        tracing::warn!(
                            error = %error,
                            worker_id = %self.state.worker_id.0,
                            "failed to answer Session Worker terminal capability query"
                        );
                        break;
                    }
                }
            }
            ReaderEvent::Eof => {
                self.pty_eof = true;
                self.state.pty_eof = true;
                self.publish();
            }
            ReaderEvent::Failed => {
                self.pty_eof = true;
                self.state.pty_eof = true;
                if !self.state.state.is_terminal()
                    && self.state.state != SessionWorkerState::Stopping
                {
                    self.transition(
                        SessionWorkerState::Failed,
                        Some("SESSION_WORKER_PTY_FAILED"),
                    );
                    if let Err(error) = self.process.terminate() {
                        tracing::warn!(
                            error = %error,
                            worker_id = %self.state.worker_id.0,
                            "failed to terminate Session Worker after PTY reader failure"
                        );
                    }
                    self.stop_deadline = Some(Instant::now() + STOP_GRACE);
                } else {
                    self.publish();
                }
            }
        }
    }

    fn poll_child(&mut self) {
        if self.child_exit.is_some() {
            return;
        }
        match self.process.try_wait() {
            Ok(Some(status)) => {
                self.child_exit = Some(ProcessExit {
                    success: status.success(),
                    exit_code: status.exit_code(),
                    signal: status.signal().map(str::to_owned),
                });
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(error = %error, worker_id = %self.state.worker_id.0, "failed to poll Session Worker child");
                self.child_exit = Some(ProcessExit {
                    success: false,
                    exit_code: 1,
                    signal: None,
                });
                self.state.error_code = Some("SESSION_WORKER_WAIT_FAILED".into());
            }
        }
    }

    fn expire_detached_lease(&mut self) {
        let Some(owner) = self.input_lease.attachment_id.clone() else {
            return;
        };
        let expired = self.attachments.get(&owner).is_none_or(|attachment| {
            !attachment.connected
                && attachment
                    .disconnected_at
                    .is_some_and(|at| at.elapsed() >= DETACH_GRACE)
        });
        if expired {
            let Some(lease_id) = self.input_lease.lease_id.clone() else {
                return;
            };
            let next_version = if let Some(persistence) = &self.persistence {
                match persistence.writer.release_input_lease(
                    &lease_id,
                    &persistence.worker_id,
                    &owner,
                    i64::try_from(self.input_lease.version).unwrap_or(i64::MAX),
                ) {
                    Ok(PersistInputLease::Released { version }) => {
                        u64::try_from(version).unwrap_or(self.input_lease.version)
                    }
                    Ok(PersistInputLease::Acquired { .. })
                    | Ok(PersistInputLease::Conflict { .. })
                    | Err(_) => return,
                }
            } else {
                self.input_lease.version.saturating_add(1)
            };
            self.input_lease.release_to_version(next_version);
            if let Some(persistence) = &self.persistence {
                persistence.proxy_owner.set(None);
            }
            self.publish();
        }
    }

    fn enforce_readiness_timeout(&mut self) {
        if self.state.state != SessionWorkerState::Connecting
            || self
                .readiness_deadline
                .is_none_or(|deadline| Instant::now() < deadline)
        {
            return;
        }
        self.readiness_deadline = None;
        self.transition(
            SessionWorkerState::Failed,
            Some("SESSION_WORKER_READINESS_TIMEOUT"),
        );
        if let Err(error) = self.process.terminate() {
            tracing::warn!(
                error = %error,
                worker_id = %self.state.worker_id.0,
                "failed to terminate Session Worker after readiness timeout"
            );
        }
        self.stop_deadline = Some(Instant::now() + STOP_GRACE);
    }

    fn cleanup_attachments(&mut self) {
        let input_owner = self.input_lease.attachment_id.as_deref();
        let now = Instant::now();
        let expired = self
            .attachments
            .iter()
            .filter(|(attachment_id, attachment)| {
                !retain_attachment(attachment_id, attachment, input_owner, now)
            })
            .map(|(attachment_id, attachment)| {
                (attachment_id.clone(), attachment.principal.clone())
            })
            .collect::<Vec<_>>();
        for (attachment_id, principal) in expired {
            if let Some(persistence) = &self.persistence
                && let Err(error) = persistence.writer.transition_terminal_attachment(
                    &attachment_id,
                    &principal,
                    "expired",
                )
            {
                tracing::warn!(
                    worker_id = %self.state.worker_id.0,
                    attachment_id,
                    error = %format!("{error:#}"),
                    "failed to persist terminal attachment expiry"
                );
                continue;
            }
            self.attachments.remove(&attachment_id);
        }
    }

    fn transition(&mut self, state: SessionWorkerState, error_code: Option<&str>) {
        self.state.state = state;
        if let Some(error_code) = error_code {
            self.state.error_code = Some(error_code.into());
        }
        self.publish();
    }

    fn publish(&mut self) {
        self.state.input_lease = self.input_lease.view();
        self.state.output_seq = self.journal.last_seq();
        self.state.terminal_retained_bytes = self.journal.retained_bytes();
        self.state.terminal_checkpoint_bytes = self.journal.checkpoint_bytes();
        self.state.pty_eof = self.pty_eof;
        self.state_sender.send_replace(self.state.clone());
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn read_pty(mut reader: Box<dyn Read + Send>, sender: SyncSender<ReaderEvent>) {
    let mut buffer = vec![0; OUTPUT_READ_BYTES];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                let _ = sender.send(ReaderEvent::Eof);
                return;
            }
            Ok(read) => {
                if sender
                    .send(ReaderEvent::Output(buffer[..read].to_vec()))
                    .is_err()
                {
                    return;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                let _ = sender.send(ReaderEvent::Failed);
                return;
            }
        }
    }
}

#[derive(Clone)]
pub struct SessionRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    runtime_root: RuntimeRoot,
    fixture_cli: Option<PathBuf>,
    real_cli: Option<PathBuf>,
    sources: HashMap<String, SessionSource>,
    writer: Option<WriterHandle>,
    database: Option<Arc<Database>>,
    fingerprint_key: [u8; 32],
    workers: RwLock<HashMap<String, SessionWorkerHandle>>,
    proxies: Mutex<HashMap<String, ProxyHandle>>,
    creates: Mutex<HashMap<(String, String), (blake3::Hash, String)>>,
}

impl SessionRegistry {
    #[cfg(test)]
    pub fn new(runtime_root: RuntimeRoot, fixture_cli: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                runtime_root,
                fixture_cli,
                real_cli: None,
                sources: HashMap::new(),
                writer: None,
                database: None,
                fingerprint_key: [0; 32],
                workers: RwLock::new(HashMap::new()),
                proxies: Mutex::new(HashMap::new()),
                creates: Mutex::new(HashMap::new()),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_runtime(
        runtime_root: RuntimeRoot,
        fixture_cli: Option<PathBuf>,
        real_cli: Option<PathBuf>,
        sources: Vec<SessionSource>,
        writer: WriterHandle,
        database: Arc<Database>,
        fingerprint_key: [u8; 32],
    ) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                runtime_root,
                fixture_cli,
                real_cli,
                sources: sources
                    .into_iter()
                    .map(|source| (source.source_id.clone(), source))
                    .collect(),
                writer: Some(writer),
                database: Some(database),
                fingerprint_key,
                workers: RwLock::new(HashMap::new()),
                proxies: Mutex::new(HashMap::new()),
                creates: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub async fn create_fake(
        &self,
        principal: String,
        idempotency_key: String,
        request: CreateFakeSession,
    ) -> Result<CreateFakeOutcome, SessionError> {
        let registry = self.clone();
        tokio::task::spawn_blocking(move || {
            registry.create_fake_blocking(principal, idempotency_key, request)
        })
        .await
        .map_err(|_| {
            SessionError::new(
                "SESSION_WORKER_SPAWN_FAILED",
                "Session Worker spawn task failed",
            )
        })?
    }

    fn create_fake_blocking(
        &self,
        principal: String,
        idempotency_key: String,
        request: CreateFakeSession,
    ) -> Result<CreateFakeOutcome, SessionError> {
        let payload = format!(
            "{}\0{}\0{}",
            request.cwd.display(),
            request.rows,
            request.cols
        );
        let payload_hash = blake3::hash(payload.as_bytes());
        let create_key = (principal, idempotency_key);
        let mut creates = self.inner.creates.lock().expect("create registry poisoned");
        if let Some((existing_hash, worker_id)) = creates.get(&create_key) {
            if existing_hash != &payload_hash {
                return Err(SessionError::new(
                    "IDEMPOTENCY_CONFLICT",
                    "Idempotency-Key is already bound to another fake session request",
                ));
            }
            let worker = self.get(worker_id).ok_or_else(|| {
                SessionError::new("SESSION_NOT_FOUND", "Session Worker not found")
            })?;
            return Ok(CreateFakeOutcome {
                snapshot: worker.snapshot(),
                replayed: true,
            });
        }
        if !valid_terminal_size(request.rows, request.cols) {
            return Err(SessionError::new(
                "TERMINAL_SIZE_INVALID",
                "terminal dimensions are outside supported bounds",
            ));
        }
        let fixture_cli = self
            .inner
            .fixture_cli
            .as_ref()
            .ok_or_else(SessionError::unavailable)?;
        let executable =
            canonical_executable(fixture_cli).map_err(|_| SessionError::unavailable())?;
        let canonical_cwd = std::fs::canonicalize(&request.cwd).map_err(|_| {
            SessionError::new(
                "CWD_INVALID",
                "session cwd does not exist or cannot be resolved",
            )
        })?;
        if !canonical_cwd.is_dir() {
            return Err(SessionError::new(
                "CWD_INVALID",
                "session cwd is not a directory",
            ));
        }
        let id = SessionWorkerId(Uuid::new_v4().to_string());
        let runtime_dir = self
            .inner
            .runtime_root
            .create_worker_dir(&id.0)
            .map_err(|_| {
                SessionError::new(
                    "SESSION_WORKER_SPAWN_FAILED",
                    "failed to create private Session Worker runtime",
                )
            })?;
        let worker = SessionWorkerHandle::spawn(
            id.clone(),
            SpawnSpec {
                executable,
                argv: Vec::new(),
                canonical_cwd,
                env_allowlist: safe_worker_environment(),
                rows: request.rows,
                cols: request.cols,
            },
            WorkerReadiness::OutputMarker,
            READINESS_TIMEOUT,
            self.inner.runtime_root.clone(),
            runtime_dir,
            None,
        )?;
        self.inner
            .workers
            .write()
            .expect("worker registry poisoned")
            .insert(id.0.clone(), worker.clone());
        creates.insert(create_key, (payload_hash, id.0));
        Ok(CreateFakeOutcome {
            snapshot: worker.snapshot(),
            replayed: false,
        })
    }

    pub async fn create(
        &self,
        principal: String,
        idempotency_key: String,
        request: CreateSession,
    ) -> Result<CreateSessionOutcome, SessionError> {
        if !matches!(request.mode.as_str(), "new" | "resume")
            || (request.mode == "resume") != request.codex_thread_id.is_some()
        {
            return Err(SessionError::new(
                "SESSION_MODE_INVALID",
                "new sessions omit codexThreadId and resume sessions require it",
            ));
        }
        if !valid_terminal_size(request.rows, request.cols) {
            return Err(SessionError::new(
                "TERMINAL_SIZE_INVALID",
                "terminal dimensions are outside supported bounds",
            ));
        }
        let source = self
            .inner
            .sources
            .get(&request.source_id)
            .cloned()
            .ok_or_else(|| SessionError::new("SOURCE_NOT_LIVE", "Session source is unavailable"))?;
        let executable = self
            .inner
            .real_cli
            .clone()
            .ok_or_else(SessionError::unavailable)?;
        let writer = self
            .inner
            .writer
            .clone()
            .ok_or_else(SessionError::unavailable)?;
        let database = self
            .inner
            .database
            .clone()
            .ok_or_else(SessionError::unavailable)?;
        let canonical_cwd = std::fs::canonicalize(&request.cwd).map_err(|_| {
            SessionError::new(
                "CWD_INVALID",
                "session cwd does not exist or cannot be resolved",
            )
        })?;
        if !canonical_cwd.is_dir() {
            return Err(SessionError::new(
                "CWD_INVALID",
                "session cwd is not a directory",
            ));
        }
        let payload = json!({
            "sourceId":request.source_id,
            "sourceEpoch":request.source_epoch,
            "expectedSupervisorVersion":request.expected_supervisor_version,
            "mode":request.mode,
            "codexThreadId":request.codex_thread_id,
            "cwd":canonical_cwd,
            "rows":request.rows,
            "cols":request.cols,
        });
        let payload_bytes = serde_json::to_vec(&payload).map_err(|_| {
            SessionError::new(
                "SESSION_REQUEST_INVALID",
                "failed to encode session request",
            )
        })?;
        let payload_hash = blake3::hash(&payload_bytes).to_hex().to_string();
        let command_id = Uuid::now_v7().to_string();
        let cwd_fingerprint = blake3::keyed_hash(
            &self.inner.fingerprint_key,
            canonical_cwd.to_string_lossy().as_bytes(),
        )
        .to_hex()
        .to_string();
        let received = writer
            .receive_gateway_command(NewGatewayCommand {
                command_id: command_id.clone(),
                principal_id: principal.clone(),
                capability: "session.create".into(),
                idempotency_key,
                payload_hash,
                target: GatewayCommandTarget {
                    source_id: request.source_id.clone(),
                    source_epoch: request.source_epoch.clone(),
                    thread_key: request
                        .codex_thread_id
                        .as_deref()
                        .map(|thread_id| thread_key(&source.store_source_id, thread_id)),
                    codex_thread_id: request.codex_thread_id.clone(),
                    expected_turn_id: None,
                    expected_request_id: None,
                    expected_request_version: None,
                },
                input_summary_json: json!({
                    "mode":request.mode,
                    "cwdFingerprint":cwd_fingerprint,
                    "rows":request.rows,
                    "cols":request.cols,
                })
                .to_string(),
                origin: GatewayCommandOrigin::WorkerControl,
            })
            .map_err(session_store_error)?;
        let command_id = match received {
            ReceiveGatewayCommand::Conflict => {
                return Err(SessionError::new(
                    "IDEMPOTENCY_CONFLICT",
                    "Idempotency-Key is already bound to another session request",
                ));
            }
            ReceiveGatewayCommand::Existing(record) => {
                let persisted = database
                    .session_worker_for_command(&record.command_id)
                    .map_err(session_store_error)?
                    .ok_or_else(|| {
                        SessionError::new(
                            "SESSION_CREATE_INCOMPLETE",
                            "idempotent session creation has no committed Worker",
                        )
                    })?;
                let worker = self.get(&persisted.worker_id).ok_or_else(|| {
                    SessionError::new(
                        "SESSION_RESTART_REQUIRED",
                        "the persisted Worker is not live; explicitly resume with a new key",
                    )
                })?;
                return Ok(CreateSessionOutcome {
                    snapshot: worker.snapshot(),
                    replayed: true,
                    existing_owner: false,
                });
            }
            ReceiveGatewayCommand::Created(record) => record.command_id,
        };
        transition_create_command(&writer, &command_id, "authorized", None, None)
            .map_err(session_store_error)?;
        transition_create_command(&writer, &command_id, "dispatching", None, None)
            .map_err(session_store_error)?;

        let worker_id = Uuid::new_v4().to_string();
        let primary_lease_id = Uuid::new_v4().to_string();
        let reservation_id =
            (request.mode == "new").then(|| format!("reservation-{}", Uuid::new_v4()));
        let runtime_dir = self
            .inner
            .runtime_root
            .create_worker_dir(&worker_id)
            .map_err(|_| {
                SessionError::new(
                    "SESSION_WORKER_SPAWN_FAILED",
                    "failed to create private Session Worker runtime",
                )
            })?;
        let registration = SessionWorkerRegistration {
            worker_id: worker_id.clone(),
            create_command_id: command_id.clone(),
            principal_id: principal.clone(),
            source_id: request.source_id.clone(),
            source_epoch: request.source_epoch.clone(),
            mode: request.mode.clone(),
            canonical_cwd: canonical_cwd.to_string_lossy().into_owned(),
            rows: request.rows,
            cols: request.cols,
            runtime_dir_name: worker_id.clone(),
            primary_lease_id: primary_lease_id.clone(),
            codex_thread_id: request.codex_thread_id.clone(),
            reservation_id,
        };
        match writer
            .register_session_worker(registration)
            .map_err(session_store_error)?
        {
            RegisterSessionWorker::ThreadOwned {
                worker_id: existing_worker_id,
            } => {
                let _ = self.inner.runtime_root.cleanup_worker_dir(&runtime_dir);
                transition_create_command(
                    &writer,
                    &command_id,
                    "accepted_by_source",
                    None,
                    Some(json!({"workerId":existing_worker_id,"attached":true}).to_string()),
                )
                .map_err(session_store_error)?;
                transition_create_command(
                    &writer,
                    &command_id,
                    "completed",
                    None,
                    Some(json!({"workerId":existing_worker_id}).to_string()),
                )
                .map_err(session_store_error)?;
                let deadline = Instant::now() + Duration::from_secs(10);
                let worker = loop {
                    if let Some(worker) = self.get(&existing_worker_id) {
                        break worker;
                    }
                    let persisted = database
                        .session_worker(&existing_worker_id)
                        .map_err(session_store_error)?
                        .ok_or_else(|| {
                            SessionError::new(
                                "THREAD_ALREADY_OWNED",
                                "Thread owner disappeared during Session startup",
                            )
                        })?;
                    if matches!(
                        persisted.state.as_str(),
                        "failed" | "exited" | "stale_epoch" | "orphaned"
                    ) {
                        return Err(SessionError::new(
                            "SESSION_RESTART_REQUIRED",
                            "the existing Thread owner did not finish starting",
                        ));
                    }
                    if Instant::now() >= deadline {
                        return Err(SessionError::new(
                            "SESSION_WORKER_NOT_READY",
                            "the existing Thread owner is still starting",
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                };
                return Ok(CreateSessionOutcome {
                    snapshot: worker.snapshot(),
                    replayed: false,
                    existing_owner: true,
                });
            }
            RegisterSessionWorker::Created { .. } => {}
        }

        let bridge = Arc::new(SessionProtocolBridge::default());
        let sink = Arc::new(SessionProxyEventSink::new(
            writer.clone(),
            database.clone(),
            self.inner.fingerprint_key,
            bridge.clone(),
            worker_id.clone(),
            primary_lease_id,
            command_id.clone(),
            request.codex_thread_id.clone(),
        ));
        let private_socket = runtime_dir.join("app-server.sock");
        let proxy = match ProxyServer::bind(ProxyConfig {
            worker_id: worker_id.clone(),
            source_id: request.source_id.clone(),
            store_source_id: source.store_source_id.clone(),
            source_epoch: request.source_epoch.clone(),
            upstream_socket: source.upstream_socket.clone(),
            private_socket: private_socket.clone(),
            event_sink: sink,
        }) {
            Ok(proxy) => proxy,
            Err(error) => {
                tracing::error!(
                    worker_id,
                    error = %format!("{error:#}"),
                    "failed to bind private Session Worker proxy"
                );
                fail_session_before_write(
                    &writer,
                    &worker_id,
                    &command_id,
                    "SESSION_PROXY_BIND_FAILED",
                );
                let _ = self.inner.runtime_root.cleanup_worker_dir(&runtime_dir);
                return Err(SessionError::new(
                    "SESSION_PROXY_BIND_FAILED",
                    "failed to create private Session Worker proxy",
                ));
            }
        };
        proxy.set_input_owner(Some(ProxyOwner {
            owner_type: "gateway".into(),
            owner_id: principal.clone(),
            principal_id: principal.clone(),
            input_lease_id: None,
        }));
        let proxy_owner = proxy.owner_handle();
        let remote = OsString::from(format!("unix://{}", private_socket.display()));
        let argv = real_worker_argv(
            &request.mode,
            remote,
            &canonical_cwd,
            request.codex_thread_id.as_deref(),
        );
        let worker = match SessionWorkerHandle::spawn(
            SessionWorkerId(worker_id.clone()),
            SpawnSpec {
                executable,
                argv,
                canonical_cwd,
                env_allowlist: real_worker_environment(&source.codex_home),
                rows: request.rows,
                cols: request.cols,
            },
            WorkerReadiness::AppServerProtocol,
            READINESS_TIMEOUT,
            self.inner.runtime_root.clone(),
            runtime_dir,
            Some(SessionPersistence {
                writer: writer.clone(),
                worker_id: worker_id.clone(),
                proxy_owner,
                initial_input_lease_version: 0,
            }),
        ) {
            Ok(worker) => worker,
            Err(error) => {
                fail_session_before_write(
                    &writer,
                    &worker_id,
                    &command_id,
                    "SESSION_WORKER_SPAWN_FAILED",
                );
                drop(proxy);
                return Err(error);
            }
        };
        let Some(worker_pid) = worker.snapshot().pid else {
            fail_session_before_write(
                &writer,
                &worker_id,
                &command_id,
                "SESSION_WORKER_PID_UNAVAILABLE",
            );
            let _ = worker.stop().await;
            drop(proxy);
            return Err(SessionError::new(
                "SESSION_WORKER_PID_UNAVAILABLE",
                "failed to identify the Session Worker child process",
            ));
        };
        if let Err(error) = proxy.authorize_downstream_pid(worker_pid) {
            tracing::error!(worker_id, error = %error, "failed to authorize private proxy downstream");
            fail_session_before_write(
                &writer,
                &worker_id,
                &command_id,
                "SESSION_PROXY_PEER_AUTH_FAILED",
            );
            let _ = worker.stop().await;
            drop(proxy);
            return Err(SessionError::new(
                "SESSION_PROXY_PEER_AUTH_FAILED",
                "failed to authorize the Session Worker proxy peer",
            ));
        }
        if let Some(persisted) = database
            .session_worker(&worker_id)
            .map_err(session_store_error)?
        {
            writer
                .transition_session_worker(crate::domain::session::SessionWorkerTransition {
                    worker_id: worker_id.clone(),
                    expected_version: persisted.version,
                    to_state: persisted.state,
                    pid: worker.snapshot().pid,
                    primary_thread_id: None,
                    error_code: persisted.error_code,
                    reason_code: Some("pty_spawned".into()),
                    command_id: Some(command_id),
                })
                .map_err(session_store_error)?;
        }
        bridge.bind(worker.clone());
        self.inner
            .workers
            .write()
            .expect("worker registry poisoned")
            .insert(worker_id.clone(), worker.clone());
        self.inner
            .proxies
            .lock()
            .expect("proxy registry poisoned")
            .insert(worker_id, proxy);
        Ok(CreateSessionOutcome {
            snapshot: worker.snapshot(),
            replayed: false,
            existing_owner: false,
        })
    }

    pub fn get(&self, worker_id: &str) -> Option<SessionWorkerHandle> {
        self.inner
            .workers
            .read()
            .expect("worker registry poisoned")
            .get(worker_id)
            .cloned()
    }

    pub fn view(&self, worker_id: &str) -> Result<SessionView, SessionError> {
        let worker = self
            .get(worker_id)
            .ok_or_else(|| SessionError::new("SESSION_NOT_FOUND", "Session Worker not found"))?;
        let Some(database) = &self.inner.database else {
            return Ok(SessionView {
                snapshot: worker.snapshot(),
                persisted_worker: None,
                thread_leases: Vec::new(),
                terminal_attachments: Vec::new(),
                active_turns: Vec::new(),
                persisted_input_lease: None,
            });
        };
        Ok(SessionView {
            snapshot: worker.snapshot(),
            persisted_worker: database
                .session_worker(worker_id)
                .map_err(session_store_error)?,
            thread_leases: database
                .thread_leases_for_worker(worker_id)
                .map_err(session_store_error)?,
            terminal_attachments: database
                .terminal_attachments_for_worker(worker_id)
                .map_err(session_store_error)?,
            active_turns: database
                .active_turn_owners_for_worker(worker_id)
                .map_err(session_store_error)?,
            persisted_input_lease: database
                .active_input_lease(worker_id)
                .map_err(session_store_error)?,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn interrupt(
        &self,
        principal_id: String,
        idempotency_key: String,
        worker_id: &str,
        source_epoch: &str,
        codex_thread_id: &str,
        expected_turn_id: &str,
        expected_worker_version: i64,
    ) -> Result<GatewayCommandRecord, SessionError> {
        let database = self.inner.database.as_ref().ok_or_else(|| {
            SessionError::new(
                "CAPABILITY_UNAVAILABLE",
                "persistent Session Kernel is disabled",
            )
        })?;
        let writer = self.inner.writer.as_ref().ok_or_else(|| {
            SessionError::new(
                "CAPABILITY_UNAVAILABLE",
                "persistent Session Kernel is disabled",
            )
        })?;
        let worker = database
            .session_worker(worker_id)
            .map_err(session_store_error)?
            .ok_or_else(|| SessionError::new("SESSION_NOT_FOUND", "Session Worker not found"))?;
        if worker.source_epoch != source_epoch {
            return Err(SessionError::new(
                "SOURCE_EPOCH_STALE",
                "Session Worker belongs to a different source epoch",
            ));
        }
        if worker.version != expected_worker_version {
            return Err(SessionError::new(
                "WORKER_VERSION_CONFLICT",
                format!(
                    "Session Worker version is {}; refresh before interrupting",
                    worker.version
                ),
            ));
        }
        if !matches!(worker.state.as_str(), "ready" | "detached") {
            return Err(SessionError::new(
                "SESSION_WORKER_NOT_READY",
                "Session Worker is not ready for interrupt",
            ));
        }
        let turn_owner = database
            .active_turn_owner(
                &worker.source_id,
                &worker.source_epoch,
                codex_thread_id,
                expected_turn_id,
            )
            .map_err(session_store_error)?
            .filter(|owner| owner.worker_id == worker_id)
            .ok_or_else(|| {
                SessionError::new(
                    "TURN_STATE_CONFLICT",
                    "expected Turn is not active on this Session Worker",
                )
            })?;
        let source = self.inner.sources.get(&worker.source_id).ok_or_else(|| {
            SessionError::new("SOURCE_NOT_CONFIGURED", "Session source is missing")
        })?;
        let command_id = Uuid::now_v7().to_string();
        let payload_hash = blake3::hash(
            format!("interrupt:{worker_id}:{codex_thread_id}:{expected_turn_id}").as_bytes(),
        )
        .to_hex()
        .to_string();
        let received = writer
            .receive_gateway_command(NewGatewayCommand {
                command_id: command_id.clone(),
                principal_id: principal_id.clone(),
                capability: "turn.interrupt".into(),
                idempotency_key,
                payload_hash,
                target: GatewayCommandTarget {
                    source_id: worker.source_id.clone(),
                    source_epoch: worker.source_epoch.clone(),
                    thread_key: Some(thread_key(&source.store_source_id, codex_thread_id)),
                    codex_thread_id: Some(codex_thread_id.into()),
                    expected_turn_id: Some(expected_turn_id.into()),
                    expected_request_id: None,
                    expected_request_version: None,
                },
                input_summary_json: json!({
                    "workerId":worker_id,
                    "workerVersion":worker.version,
                    "expectedTurnId":expected_turn_id,
                    "turnOwnerPrincipalId":turn_owner.principal_id,
                    "turnOwnerType":turn_owner.owner_type,
                    "actorPrincipalId":principal_id,
                })
                .to_string(),
                origin: GatewayCommandOrigin::WorkerControl,
            })
            .map_err(session_store_error)?;
        match received {
            ReceiveGatewayCommand::Existing(record) => return Ok(record),
            ReceiveGatewayCommand::Conflict => {
                return Err(SessionError::new(
                    "IDEMPOTENCY_CONFLICT",
                    "idempotency key was already used with different interrupt parameters",
                ));
            }
            ReceiveGatewayCommand::Created(_) => {}
        }
        transition_create_command(writer, &command_id, "authorized", None, None)
            .map_err(session_store_error)?;
        transition_create_command(writer, &command_id, "dispatching", None, None)
            .map_err(session_store_error)?;
        let proxy = self
            .inner
            .proxies
            .lock()
            .expect("proxy registry poisoned")
            .get(worker_id)
            .map(ProxyHandle::control_handle);
        let Some(proxy) = proxy else {
            transition_create_command(
                writer,
                &command_id,
                "failed",
                Some("SESSION_PROXY_UNAVAILABLE"),
                None,
            )
            .map_err(session_store_error)?;
            return Err(SessionError::new(
                "SESSION_PROXY_UNAVAILABLE",
                "Session proxy is unavailable",
            ));
        };
        if let Err(error) = proxy
            .interrupt(
                codex_thread_id.into(),
                expected_turn_id.into(),
                crate::session::proxy::MutationBoundary {
                    command_id: command_id.clone(),
                },
            )
            .await
        {
            tracing::warn!(worker_id, command_id, error = %error, "Session interrupt outcome is unavailable");
            return database
                .gateway_command(&command_id)
                .map_err(session_store_error)?
                .ok_or_else(|| {
                    SessionError::new(
                        "SESSION_PERSISTENCE_FAILED",
                        "interrupt command disappeared",
                    )
                });
        }
        database
            .gateway_command(&command_id)
            .map_err(session_store_error)?
            .ok_or_else(|| {
                SessionError::new(
                    "SESSION_PERSISTENCE_FAILED",
                    "interrupt command disappeared",
                )
            })
    }

    pub async fn resolve_pending_request(
        &self,
        command_id: &str,
        source_id: &str,
        source_epoch: &str,
        codex_thread_id: &str,
        request_id: &str,
        action: PendingRequestAction,
    ) -> Result<GatewayCommandRecord, SessionError> {
        let database = self
            .inner
            .database
            .as_ref()
            .ok_or_else(SessionError::unavailable)?;
        let writer = self
            .inner
            .writer
            .as_ref()
            .ok_or_else(SessionError::unavailable)?;
        let worker_id = database
            .active_thread_lease_owner(source_id, source_epoch, codex_thread_id)
            .map_err(session_store_error)?
            .ok_or_else(|| {
                SessionError::new("THREAD_NOT_LOADED", "Thread has no active Session Worker")
            })?;
        let control = self
            .inner
            .proxies
            .lock()
            .expect("proxy registry poisoned")
            .get(&worker_id)
            .map(ProxyHandle::control_handle)
            .ok_or_else(|| {
                SessionError::new("SESSION_PROXY_UNAVAILABLE", "Session proxy is unavailable")
            })?;
        let response = control
            .prepare_request_action(request_id.to_string(), action)
            .await
            .map_err(|error| {
                let code = error.to_string();
                SessionError::new(
                    if code.contains("REQUEST_OWNED_BY_TERMINAL") {
                        "REQUEST_OWNED_BY_TERMINAL"
                    } else if code.contains("REQUEST_OWNER_UNATTRIBUTED") {
                        "REQUEST_OWNER_UNATTRIBUTED"
                    } else if code.contains("REQUEST_NOT_PENDING") {
                        "REQUEST_NOT_PENDING"
                    } else {
                        "COMMAND_INVALID"
                    },
                    "pending request action is not available for this owner",
                )
            })?;
        match writer
            .claim_pending_request(command_id)
            .map_err(session_store_error)?
        {
            PendingRequestClaim::Claimed(_) => {}
            PendingRequestClaim::AlreadyResolved => {
                return Err(SessionError::new(
                    "REQUEST_ALREADY_RESOLVED",
                    "another client already resolved this request",
                ));
            }
            PendingRequestClaim::SourceEpochStale => {
                return Err(SessionError::new(
                    "SOURCE_EPOCH_STALE",
                    "the pending request belongs to a stale source epoch",
                ));
            }
            PendingRequestClaim::NotPending => {
                return Err(SessionError::new(
                    "REQUEST_NOT_PENDING",
                    "the pending request version no longer matches",
                ));
            }
        }
        if let Err(error) = control
            .resolve_request(
                request_id.to_string(),
                response,
                crate::session::proxy::MutationBoundary {
                    command_id: command_id.to_string(),
                },
            )
            .await
        {
            let code = if error.to_string().contains("UPSTREAM_DISCONNECTED")
                || error.to_string().contains("OUTCOME_UNKNOWN")
            {
                "OUTCOME_UNKNOWN"
            } else {
                "REQUEST_NOT_PENDING"
            };
            return Err(SessionError::new(
                code,
                "pending request resolution was not confirmed",
            ));
        }
        database
            .gateway_command(command_id)
            .map_err(session_store_error)?
            .ok_or_else(|| {
                SessionError::new("SESSION_PERSISTENCE_FAILED", "request command disappeared")
            })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn stop_session(
        &self,
        principal_id: String,
        idempotency_key: String,
        worker_id: &str,
        source_epoch: &str,
        expected_worker_version: i64,
        active_turn_policy: &str,
        mut expected_active_turns: Vec<ExpectedActiveTurn>,
    ) -> Result<StopSessionOutcome, SessionError> {
        let database = self
            .inner
            .database
            .as_ref()
            .ok_or_else(SessionError::unavailable)?;
        let writer = self
            .inner
            .writer
            .as_ref()
            .ok_or_else(SessionError::unavailable)?;
        let persisted = database
            .session_worker(worker_id)
            .map_err(session_store_error)?
            .ok_or_else(|| SessionError::new("SESSION_NOT_FOUND", "Session Worker not found"))?;
        if persisted.source_epoch != source_epoch {
            return Err(SessionError::new(
                "SOURCE_EPOCH_STALE",
                "Session Worker belongs to a different source epoch",
            ));
        }
        let live_worker = self.get(worker_id).ok_or_else(|| {
            SessionError::new(
                "SESSION_RESTART_REQUIRED",
                "the persisted Session Worker is not live",
            )
        })?;

        let payload_hash = blake3::hash(
            serde_json::to_string(&json!({
                "workerId":worker_id,
                "sourceEpoch":source_epoch,
                "expectedWorkerVersion":expected_worker_version,
                "activeTurnPolicy":active_turn_policy,
                "expectedActiveTurns":expected_active_turns,
            }))
            .map_err(|_| SessionError::new("COMMAND_INVALID", "stop request is invalid"))?
            .as_bytes(),
        )
        .to_hex()
        .to_string();
        let command_id = Uuid::now_v7().to_string();
        let received = writer
            .receive_gateway_command(NewGatewayCommand {
                command_id: command_id.clone(),
                principal_id: principal_id.clone(),
                capability: "session.stop".into(),
                idempotency_key,
                payload_hash,
                target: GatewayCommandTarget {
                    source_id: persisted.source_id.clone(),
                    source_epoch: persisted.source_epoch.clone(),
                    thread_key: None,
                    codex_thread_id: persisted.primary_thread_id.clone(),
                    expected_turn_id: None,
                    expected_request_id: None,
                    expected_request_version: None,
                },
                input_summary_json: json!({
                    "workerId":worker_id,
                    "expectedWorkerVersion":expected_worker_version,
                    "activeTurnPolicy":active_turn_policy,
                    "expectedActiveTurnCount":expected_active_turns.len(),
                })
                .to_string(),
                origin: GatewayCommandOrigin::WorkerControl,
            })
            .map_err(session_store_error)?;
        match received {
            ReceiveGatewayCommand::Existing(command) => {
                return Ok(StopSessionOutcome {
                    snapshot: live_worker.snapshot(),
                    command,
                });
            }
            ReceiveGatewayCommand::Conflict => {
                return Err(SessionError::new(
                    "IDEMPOTENCY_CONFLICT",
                    "idempotency key was already used with different stop parameters",
                ));
            }
            ReceiveGatewayCommand::Created(_) => {}
        }

        if persisted.version != expected_worker_version {
            transition_create_command(
                writer,
                &command_id,
                "rejected",
                Some("WORKER_VERSION_CONFLICT"),
                None,
            )
            .map_err(session_store_error)?;
            return Err(SessionError::new(
                "WORKER_VERSION_CONFLICT",
                format!(
                    "Session Worker version is {}; refresh before stopping",
                    persisted.version
                ),
            ));
        }
        if !matches!(
            persisted.state.as_str(),
            "starting" | "connecting" | "ready" | "detached"
        ) {
            transition_create_command(
                writer,
                &command_id,
                "rejected",
                Some("SESSION_WORKER_NOT_READY"),
                None,
            )
            .map_err(session_store_error)?;
            return Err(SessionError::new(
                "SESSION_WORKER_NOT_READY",
                "Session Worker cannot be stopped from its current state",
            ));
        }

        let active_turns = database
            .active_turn_owners_for_worker(worker_id)
            .map_err(session_store_error)?;
        expected_active_turns.sort_by(|left, right| {
            (&left.thread_id, &left.turn_id).cmp(&(&right.thread_id, &right.turn_id))
        });
        expected_active_turns.dedup();
        let mut actual_turns = active_turns
            .iter()
            .map(|turn| ExpectedActiveTurn {
                thread_id: turn.codex_thread_id.clone(),
                turn_id: turn.codex_turn_id.clone(),
            })
            .collect::<Vec<_>>();
        actual_turns.sort_by(|left, right| {
            (&left.thread_id, &left.turn_id).cmp(&(&right.thread_id, &right.turn_id))
        });
        let policy_valid = match active_turn_policy {
            "reject_if_active" => actual_turns.is_empty() && expected_active_turns.is_empty(),
            "interrupt_expected" => {
                !actual_turns.is_empty() && expected_active_turns == actual_turns
            }
            _ => false,
        };
        if !policy_valid {
            let code = if matches!(
                active_turn_policy,
                "reject_if_active" | "interrupt_expected"
            ) {
                "TURN_STATE_CONFLICT"
            } else {
                "COMMAND_INVALID"
            };
            transition_create_command(writer, &command_id, "rejected", Some(code), None)
                .map_err(session_store_error)?;
            return Err(SessionError::new(
                code,
                "active Turn state does not match the requested stop policy",
            ));
        }

        transition_create_command(writer, &command_id, "authorized", None, None)
            .map_err(session_store_error)?;
        transition_create_command(writer, &command_id, "dispatching", None, None)
            .map_err(session_store_error)?;

        if active_turn_policy == "interrupt_expected" {
            for turn in &actual_turns {
                let interrupt_key = format!("{command_id}:{}:{}", turn.thread_id, turn.turn_id);
                let interrupt = match self
                    .interrupt(
                        principal_id.clone(),
                        interrupt_key,
                        worker_id,
                        source_epoch,
                        &turn.thread_id,
                        &turn.turn_id,
                        expected_worker_version,
                    )
                    .await
                {
                    Ok(interrupt) => interrupt,
                    Err(error) => {
                        transition_create_command(
                            writer,
                            &command_id,
                            "failed",
                            Some("STOP_INTERRUPT_FAILED"),
                            None,
                        )
                        .map_err(session_store_error)?;
                        return Err(error);
                    }
                };
                if interrupt.state == "outcome_unknown" {
                    transition_create_command(
                        writer,
                        &command_id,
                        "outcome_unknown",
                        Some("STOP_INTERRUPT_OUTCOME_UNKNOWN"),
                        None,
                    )
                    .map_err(session_store_error)?;
                    return Err(SessionError::new(
                        "OUTCOME_UNKNOWN",
                        "an expected active Turn interrupt may have reached the source",
                    ));
                }
                if !matches!(
                    interrupt.state.as_str(),
                    "completed" | "accepted_by_source" | "running"
                ) {
                    transition_create_command(
                        writer,
                        &command_id,
                        "failed",
                        Some("STOP_INTERRUPT_FAILED"),
                        None,
                    )
                    .map_err(session_store_error)?;
                    return Err(SessionError::new(
                        "STOP_INTERRUPT_FAILED",
                        "an expected active Turn could not be interrupted",
                    ));
                }
            }
        }

        if let Err(error) = writer.transition_session_worker(SessionWorkerTransition {
            worker_id: worker_id.into(),
            expected_version: persisted.version,
            to_state: "stopping".into(),
            pid: None,
            primary_thread_id: None,
            error_code: None,
            reason_code: Some("stop_requested".into()),
            command_id: Some(command_id.clone()),
        }) {
            let _ = transition_create_command(
                writer,
                &command_id,
                "failed",
                Some("SESSION_PERSISTENCE_FAILED"),
                None,
            );
            return Err(session_store_error(error));
        }

        let snapshot = match live_worker.stop().await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                transition_create_command(
                    writer,
                    &command_id,
                    "failed",
                    Some("SESSION_WORKER_STOP_FAILED"),
                    None,
                )
                .map_err(session_store_error)?;
                return Err(error);
            }
        };
        transition_create_command(
            writer,
            &command_id,
            "accepted_by_source",
            None,
            Some(json!({"workerId":worker_id,"state":&snapshot.state}).to_string()),
        )
        .map_err(session_store_error)?;
        let command = database
            .gateway_command(&command_id)
            .map_err(session_store_error)?
            .ok_or_else(|| {
                SessionError::new("SESSION_PERSISTENCE_FAILED", "stop command disappeared")
            })?;
        let writer = writer.clone();
        let completion_worker = live_worker.clone();
        let completion_command_id = command_id;
        tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let state = completion_worker.snapshot().state;
                if state.is_terminal() {
                    let (to_state, error_code) = if state == SessionWorkerState::Exited {
                        ("completed", None)
                    } else {
                        ("failed", Some("SESSION_WORKER_STOP_FAILED"))
                    };
                    let _ = transition_create_command(
                        &writer,
                        &completion_command_id,
                        to_state,
                        error_code,
                        None,
                    );
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = transition_create_command(
                        &writer,
                        &completion_command_id,
                        "outcome_unknown",
                        Some("SESSION_WORKER_STOP_TIMEOUT"),
                        None,
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        Ok(StopSessionOutcome { snapshot, command })
    }

    pub fn fixture_available(&self) -> bool {
        self.inner
            .fixture_cli
            .as_ref()
            .is_some_and(|path| canonical_executable(path).is_ok())
    }

    pub async fn stale_source(&self, source_id: &str, source_epoch: &str) {
        let (Some(database), Some(writer)) = (&self.inner.database, &self.inner.writer) else {
            return;
        };
        let workers = self
            .inner
            .workers
            .read()
            .expect("worker registry poisoned")
            .iter()
            .filter_map(|(worker_id, worker)| {
                let persisted = database.session_worker(worker_id).ok().flatten()?;
                (persisted.source_id == source_id && persisted.source_epoch == source_epoch)
                    .then(|| (worker_id.clone(), worker.clone()))
            })
            .collect::<Vec<_>>();
        for (worker_id, worker) in &workers {
            if let Err(error) = writer.finalize_session_worker(
                worker_id,
                "stale_epoch",
                Some("SOURCE_EPOCH_STALE"),
                "source_epoch_rotated",
            ) {
                tracing::error!(
                    worker_id,
                    error = %format!("{error:#}"),
                    "failed to persist stale Session Worker"
                );
            }
            let _ = worker.source_stale();
        }
        let proxies = {
            let mut proxies = self.inner.proxies.lock().expect("proxy registry poisoned");
            workers
                .iter()
                .filter_map(|(worker_id, _)| proxies.remove(worker_id))
                .collect::<Vec<_>>()
        };
        for proxy in proxies {
            let _ = proxy.shutdown().await;
        }
    }

    pub async fn shutdown_all(&self) {
        let workers = self
            .inner
            .workers
            .read()
            .expect("worker registry poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for worker in &workers {
            let _ = worker.stop().await;
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline
            && workers
                .iter()
                .any(|worker| !worker.snapshot().state.is_terminal())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let proxies = self
            .inner
            .proxies
            .lock()
            .expect("proxy registry poisoned")
            .drain()
            .map(|(_, proxy)| proxy)
            .collect::<Vec<_>>();
        for proxy in proxies {
            let _ = proxy.shutdown().await;
        }
    }
}

fn transition_create_command(
    writer: &WriterHandle,
    command_id: &str,
    to_state: &str,
    error_code: Option<&str>,
    result_summary_json: Option<String>,
) -> anyhow::Result<()> {
    writer.transition_gateway_command(GatewayTransition {
        command_id: command_id.into(),
        to_state: to_state.into(),
        result_summary_json,
        error_code: error_code.map(str::to_string),
        error_message: error_code.map(|_| "Session Worker operation failed".into()),
        reason_code: error_code.map(str::to_string),
        decision: if error_code.is_some() {
            "deny"
        } else {
            "allow"
        }
        .into(),
        outcome: if error_code.is_some() {
            "failed"
        } else {
            to_state
        }
        .into(),
    })?;
    Ok(())
}

fn fail_session_before_write(
    writer: &WriterHandle,
    worker_id: &str,
    command_id: &str,
    error_code: &str,
) {
    if let Err(error) = writer.fail_session_before_write(worker_id, error_code, command_id) {
        tracing::error!(
            worker_id,
            command_id,
            error = %format!("{error:#}"),
            "failed to persist pre-write Session Worker failure"
        );
        return;
    }
    if let Err(error) =
        transition_create_command(writer, command_id, "failed", Some(error_code), None)
    {
        tracing::error!(
            worker_id,
            command_id,
            error = %format!("{error:#}"),
            "failed to complete failed Session Worker command"
        );
    }
}

fn session_store_error(error: anyhow::Error) -> SessionError {
    tracing::error!(error = %format!("{error:#}"), "Session Kernel persistence operation failed");
    SessionError::new(
        "SESSION_PERSISTENCE_FAILED",
        "Session Kernel state could not be committed",
    )
}

fn opaque_credential() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn retain_attachment(
    attachment_id: &str,
    attachment: &Attachment,
    input_owner: Option<&str>,
    now: Instant,
) -> bool {
    attachment.connected
        || input_owner == Some(attachment_id)
        || attachment.descriptor_expires_at > now
}

fn credential_hash_matches(expected: &blake3::Hash, supplied: &str) -> bool {
    if supplied.len() != 64 || !supplied.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    expected
        .as_bytes()
        .iter()
        .zip(blake3::hash(supplied.as_bytes()).as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn safe_worker_environment() -> BTreeMap<OsString, OsString> {
    let mut environment = BTreeMap::from([
        (OsString::from("TERM"), OsString::from("xterm-256color")),
        (OsString::from("COLORTERM"), OsString::from("truecolor")),
    ]);
    for key in ["LANG", "LC_ALL", "PATH"] {
        if let Some(value) = std::env::var_os(key) {
            environment.insert(OsString::from(key), value);
        }
    }
    environment
}

fn real_worker_environment(codex_home: &std::path::Path) -> BTreeMap<OsString, OsString> {
    let mut environment = safe_worker_environment();
    environment.insert(
        OsString::from("CODEX_HOME"),
        codex_home.as_os_str().to_os_string(),
    );
    for key in ["HOME", "USER", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            environment.insert(OsString::from(key), value);
        }
    }
    environment
}

fn real_worker_argv(
    mode: &str,
    remote: OsString,
    canonical_cwd: &std::path::Path,
    codex_thread_id: Option<&str>,
) -> Vec<OsString> {
    let mut argv = Vec::with_capacity(if mode == "resume" { 8 } else { 6 });
    if mode == "resume" {
        argv.push(OsString::from("resume"));
    }
    // The update prompt runs before --remote connects, when no ThreadLease exists and terminal
    // input must remain fail-closed. This fixed override only removes that startup interlock; it
    // is not supplied by the Browser and does not alter approval or sandbox policy.
    argv.extend([
        OsString::from("-c"),
        OsString::from("check_for_update_on_startup=false"),
        OsString::from("--remote"),
        remote,
        OsString::from("-C"),
        canonical_cwd.as_os_str().to_os_string(),
    ]);
    if mode == "resume" {
        argv.push(OsString::from(
            codex_thread_id.expect("resume was validated with a Thread ID"),
        ));
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::fs;
    use tempfile::TempDir;
    use tokio::net::UnixListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    #[test]
    fn real_worker_argv_disables_pre_protocol_update_prompt() {
        let cwd = PathBuf::from("/workspace");
        let remote = OsString::from("unix:///runtime/app-server.sock");
        let new_argv = real_worker_argv("new", remote.clone(), &cwd, None);
        assert_eq!(
            new_argv,
            [
                "-c",
                "check_for_update_on_startup=false",
                "--remote",
                "unix:///runtime/app-server.sock",
                "-C",
                "/workspace",
            ]
            .map(OsString::from)
        );

        let resume_argv = real_worker_argv("resume", remote, &cwd, Some("thread-1"));
        assert_eq!(
            resume_argv,
            [
                "resume",
                "-c",
                "check_for_update_on_startup=false",
                "--remote",
                "unix:///runtime/app-server.sock",
                "-C",
                "/workspace",
                "thread-1",
            ]
            .map(OsString::from)
        );
    }

    #[cfg(unix)]
    fn fake_cli(temp: &TempDir) -> Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;

        let path = temp.path().join("fake-codex");
        fs::write(
            &path,
            b"#!/bin/sh\nprintf 'SESSION_READY\\r\\n'\ntrap 'printf RESIZED\\r\\n' WINCH\nwhile IFS= read -r line; do printf 'ECHO:%s\\r\\n' \"$line\"; done\n",
        )?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(path)
    }

    #[cfg(unix)]
    async fn ready_worker() -> Result<(TempDir, SessionRegistry, SessionWorkerHandle)> {
        let temp = tempfile::Builder::new()
            .prefix("session-real-")
            .tempdir_in("/private/tmp")?;
        let fixture = fake_cli(&temp)?;
        let runtime = RuntimeRoot::prepare(temp.path().join("runtime"))?;
        let registry = SessionRegistry::new(runtime, Some(fixture));
        let created = registry
            .create_fake(
                "principal".into(),
                "idempotency-key-0001".into(),
                CreateFakeSession {
                    cwd: temp.path().to_path_buf(),
                    rows: 24,
                    cols: 80,
                },
            )
            .await?;
        let worker = registry
            .get(&created.snapshot.worker_id.0)
            .context("missing worker")?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while worker.snapshot().state != SessionWorkerState::Ready && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok((temp, registry, worker))
    }

    #[cfg(unix)]
    async fn ready_persistent_worker() -> Result<(
        TempDir,
        SessionRegistry,
        SessionWorkerHandle,
        Arc<Database>,
        WriterHandle,
        tokio::task::JoinHandle<Result<()>>,
    )> {
        let cli = canonical_executable(&PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/fake_codex_remote.sh"
        )))?;
        ready_persistent_worker_with_cli(cli, false, "new").await
    }

    #[cfg(unix)]
    async fn ready_persistent_worker_with_cli(
        cli: PathBuf,
        emulate_terminal: bool,
        mode: &str,
    ) -> Result<(
        TempDir,
        SessionRegistry,
        SessionWorkerHandle,
        Arc<Database>,
        WriterHandle,
        tokio::task::JoinHandle<Result<()>>,
    )> {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::Builder::new()
            .prefix("session-stop-")
            .tempdir_in("/private/tmp")?;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))?;
        let upstream_path = temp.path().join("app-server.sock");
        let listener = UnixListener::bind(&upstream_path)?;
        fs::set_permissions(&upstream_path, fs::Permissions::from_mode(0o600))?;
        let fixture_cwd = temp.path().to_string_lossy().into_owned();
        let fake_server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut websocket = accept_async(stream).await?;
            let mut started_threads = 0_u8;
            while let Some(message) = websocket.next().await {
                let message = match message {
                    Ok(message) => message,
                    Err(_) => break,
                };
                match message {
                    Message::Text(text) => {
                        let request: Value = serde_json::from_str(&text)?;
                        let Some(method) = request.get("method").and_then(Value::as_str) else {
                            continue;
                        };
                        let fixture_thread = json!({
                            "id":"01984de2-8f74-7c91-a3b2-5c5e937cf318",
                            "extra":null,
                            "sessionId":"01984de2-8f74-7c91-a3b2-5c5e937cf318",
                            "forkedFromId":null,
                            "parentThreadId":null,
                            "preview":"Synthetic resume thread",
                            "ephemeral":false,
                            "section":null,
                            "sectionEnteredAt":null,
                            "historyMode":"legacy",
                            "modelProvider":"openai",
                            "createdAt":1,
                            "updatedAt":1,
                            "recencyAt":1,
                            "status":{"type":"idle"},
                            "path":null,
                            "cwd":fixture_cwd,
                            "cliVersion":"0.146.1",
                            "source":"cli",
                            "canAcceptDirectInput":null,
                            "threadSource":null,
                            "agentNickname":null,
                            "agentRole":null,
                            "gitInfo":null,
                            "name":null,
                            "turns":[]
                        });
                        let lifecycle_response = |thread: Value| {
                            json!({
                                "thread":thread,
                                "model":"fixture-model",
                                "modelProvider":"openai",
                                "serviceTier":null,
                                "cwd":fixture_cwd,
                                "runtimeWorkspaceRoots":[],
                                "instructionSources":[],
                                "approvalPolicy":"on-request",
                                "approvalsReviewer":"user",
                                "sandbox":{"type":"dangerFullAccess"},
                                "activePermissionProfile":null,
                                "reasoningEffort":"low",
                                "multiAgentMode":"explicitRequestOnly"
                            })
                        };
                        let result = match method {
                            "initialize" => Some(json!({
                                "codexHome":"/synthetic/codex-home",
                                "userAgent":"codex_cli_rs/0.146.1-fixture"
                            })),
                            "account/read" => Some(json!({
                                "account":{"type":"apiKey"},
                                "requiresOpenaiAuth":false
                            })),
                            "hooks/list" => Some(json!({"data":[]})),
                            "skills/list" => Some(json!({"data":[]})),
                            "plugin/list" => Some(json!({
                                "marketplaces":[],
                                "marketplaceLoadErrors":[],
                                "featuredPluginIds":[]
                            })),
                            "model/list" => Some(json!({
                                "data":[{
                                    "id":"fixture-model",
                                    "model":"fixture-model",
                                    "upgrade":null,
                                    "upgradeInfo":null,
                                    "availabilityNux":null,
                                    "displayName":"Fixture Model",
                                    "description":"Synthetic model for Session Kernel smoke testing",
                                    "hidden":false,
                                    "supportedReasoningEfforts":[{
                                        "reasoningEffort":"low",
                                        "description":"Synthetic low effort"
                                    }],
                                    "defaultReasoningEffort":"low",
                                    "inputModalities":["text"],
                                    "supportsPersonality":false,
                                    "multiAgentVersion":null,
                                    "additionalSpeedTiers":[],
                                    "serviceTiers":[],
                                    "defaultServiceTier":null,
                                    "isDefault":true
                                }],
                                "nextCursor":null
                            })),
                            "configRequirements/read" => Some(json!({"requirements":null})),
                            "thread/list" => Some(json!({
                                "data":[fixture_thread.clone()],
                                "nextCursor":null,
                                "backwardsCursor":null
                            })),
                            "thread/read" => Some(json!({"thread":fixture_thread.clone()})),
                            "thread/start" => {
                                started_threads = started_threads.saturating_add(1);
                                let mut started_thread = fixture_thread;
                                if started_threads > 1 {
                                    started_thread["id"] =
                                        json!("01984de2-8f74-7c91-a3b2-5c5e937cf319");
                                    started_thread["sessionId"] =
                                        json!("01984de2-8f74-7c91-a3b2-5c5e937cf319");
                                    started_thread["preview"] = json!("Synthetic clear thread");
                                }
                                Some(lifecycle_response(started_thread))
                            }
                            "thread/resume" => Some(lifecycle_response(fixture_thread)),
                            "turn/interrupt" => Some(json!({})),
                            _ => None,
                        };
                        if let Some(result) = result {
                            websocket
                                .send(Message::Text(
                                    json!({"id":request["id"],"result":result})
                                        .to_string()
                                        .into(),
                                ))
                                .await?;
                        } else if request.get("id").is_some() {
                            websocket
                                .send(Message::Text(
                                    json!({
                                        "id":request["id"],
                                        "error":{"code":-32601,"message":"fixture method not found"}
                                    })
                                    .to_string()
                                    .into(),
                                ))
                                .await?;
                        }
                    }
                    Message::Ping(payload) => websocket.send(Message::Pong(payload)).await?,
                    Message::Close(_) => break,
                    Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
                }
            }
            Result::<()>::Ok(())
        });

        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let writer = WriterHandle::start(database.clone(), 256, 128, 32)?;
        writer.upsert_source_kind(
            "source-stop",
            "app_server",
            "fixture-upstream",
            &json!({}),
            "ready",
        )?;
        writer.record_live_capabilities("source-stop", "epoch-stop", &json!({}))?;
        fs::create_dir(temp.path().join("codex-home"))?;
        let registry = SessionRegistry::new_runtime(
            RuntimeRoot::prepare(temp.path().join("runtime"))?,
            None,
            Some(cli),
            vec![SessionSource {
                source_id: "source-stop".into(),
                store_source_id: "store-stop".into(),
                codex_home: temp.path().join("codex-home"),
                upstream_socket: upstream_path,
            }],
            writer.clone(),
            database.clone(),
            [6; 32],
        );
        let created = registry
            .create(
                "local_bearer".into(),
                "persistent-stop-create".into(),
                CreateSession {
                    source_id: "source-stop".into(),
                    source_epoch: "epoch-stop".into(),
                    expected_supervisor_version: 1,
                    mode: mode.into(),
                    codex_thread_id: (mode == "resume")
                        .then(|| "01984de2-8f74-7c91-a3b2-5c5e937cf318".into()),
                    cwd: temp.path().to_path_buf(),
                    rows: 24,
                    cols: 80,
                },
            )
            .await?;
        let worker = registry
            .get(&created.snapshot.worker_id.0)
            .context("missing persistent worker")?;
        let terminal_owner = if emulate_terminal {
            let attachment = worker
                .prepare_attachment("local_bearer".into(), None, None)
                .await?;
            let attachment_id = worker
                .connect_attachment("local_bearer".into(), attachment.descriptor)
                .await?;
            let lease = worker
                .acquire_input_lease(
                    "local_bearer".into(),
                    attachment_id.clone(),
                    attachment.attachment_token,
                    attachment.input_lease_version,
                    false,
                )
                .await?;
            let lease_id = lease.lease_id.context("terminal emulation lease missing")?;
            Some((attachment_id, lease_id))
        } else {
            None
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while worker.snapshot().state != SessionWorkerState::Ready
            && !worker.snapshot().state.is_terminal()
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if worker.snapshot().state != SessionWorkerState::Ready {
            let methods = database.connect()?.query_row(
                "SELECT COALESCE(group_concat(method,','),'') FROM raw_events WHERE worker_id=?1 ORDER BY event_seq",
                [&worker.id().0],
                |row| row.get::<_, String>(0),
            )?;
            anyhow::bail!(
                "persistent worker did not become ready: state={:?}, error={:?}, exit={:?}, methods={methods}",
                worker.snapshot().state,
                worker.snapshot().error_code,
                worker.snapshot().exit
            );
        }
        if mode == "new"
            && let Some((attachment_id, lease_id)) = terminal_owner
        {
            tokio::time::sleep(Duration::from_millis(250)).await;
            worker
                .write_input(
                    attachment_id.clone(),
                    lease_id.clone(),
                    b"/clear\r".to_vec(),
                )
                .await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
            worker
                .write_input(attachment_id, lease_id, b"\r".to_vec())
                .await?;
            let clear_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let primary_thread = database
                    .session_worker(&worker.id().0)?
                    .and_then(|persisted| persisted.primary_thread_id);
                if primary_thread.as_deref() == Some("01984de2-8f74-7c91-a3b2-5c5e937cf319") {
                    break;
                }
                if worker.snapshot().state.is_terminal() || Instant::now() >= clear_deadline {
                    let methods = database.connect()?.query_row(
                        "SELECT COALESCE(group_concat(method,','),'') FROM raw_events WHERE worker_id=?1 ORDER BY event_seq",
                        [&worker.id().0],
                        |row| row.get::<_, String>(0),
                    )?;
                    anyhow::bail!(
                        "real Codex /clear did not switch the primary ThreadLease: state={:?}, primary={primary_thread:?}, methods={methods}",
                        worker.snapshot().state
                    );
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        Ok((temp, registry, worker, database, writer, fake_server))
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "manual smoke: set SESSION_REAL_CODEX_CLI to an installed Codex executable"]
    async fn installed_codex_tui_new_and_resume_connect_through_private_session_proxy() -> Result<()>
    {
        let cli = std::env::var_os("SESSION_REAL_CODEX_CLI")
            .map(PathBuf::from)
            .context("SESSION_REAL_CODEX_CLI is required")?;
        let cli = canonical_executable(&cli)?;
        for mode in ["new", "resume"] {
            let (_temp, _registry, worker, database, _writer, fake_server) =
                ready_persistent_worker_with_cli(cli.clone(), true, mode).await?;
            assert_eq!(worker.snapshot().state, SessionWorkerState::Ready);
            let leases = database.thread_leases_for_worker(&worker.id().0)?;
            let expected_primary = if mode == "new" {
                "01984de2-8f74-7c91-a3b2-5c5e937cf319"
            } else {
                "01984de2-8f74-7c91-a3b2-5c5e937cf318"
            };
            assert!(leases.iter().any(|lease| {
                lease.codex_thread_id.as_deref() == Some(expected_primary)
                    && lease.state == "active"
                    && lease.role == "primary"
            }));
            worker.stop().await?;
            let deadline = Instant::now() + Duration::from_secs(4);
            while !worker.snapshot().state.is_terminal() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(worker.snapshot().state.is_terminal());
            fake_server.await??;
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persistent_stop_interrupts_exact_turn_and_replays_idempotently() -> Result<()> {
        let (_temp, registry, worker, database, writer, fake_server) =
            ready_persistent_worker().await?;
        let prepared = worker
            .prepare_attachment("local_bearer".into(), None, None)
            .await?;
        let attachment_id = worker
            .connect_attachment("local_bearer".into(), prepared.descriptor)
            .await?;
        let input_lease = worker
            .acquire_input_lease(
                "local_bearer".into(),
                attachment_id.clone(),
                prepared.attachment_token,
                prepared.input_lease_version,
                false,
            )
            .await?;
        let input_lease_id = input_lease
            .lease_id
            .clone()
            .context("persistent input lease missing")?;
        writer.upsert_turn_owner(TurnOwnerRecord {
            source_id: "source-stop".into(),
            source_epoch: "epoch-stop".into(),
            worker_id: worker.id().0.clone(),
            codex_thread_id: "01984de2-8f74-7c91-a3b2-5c5e937cf318".into(),
            codex_turn_id: "turn-stop-1".into(),
            owner_type: "terminal".into(),
            owner_id: attachment_id,
            principal_id: "local_bearer".into(),
            input_lease_id: Some(input_lease_id),
            state: "active".into(),
            version: 1,
            start_command_id: None,
        })?;
        let worker_version = database
            .session_worker(&worker.id().0)?
            .context("missing worker")?
            .version;
        let expected = vec![ExpectedActiveTurn {
            thread_id: "01984de2-8f74-7c91-a3b2-5c5e937cf318".into(),
            turn_id: "turn-stop-1".into(),
        }];
        let stopped = registry
            .stop_session(
                "local_bearer".into(),
                "persistent-stop-idempotency".into(),
                &worker.id().0,
                "epoch-stop",
                worker_version,
                "interrupt_expected",
                expected.clone(),
            )
            .await?;
        assert_eq!(stopped.snapshot.state, SessionWorkerState::Stopping);
        assert_eq!(stopped.command.state, "accepted_by_source");

        let replay = registry
            .stop_session(
                "local_bearer".into(),
                "persistent-stop-idempotency".into(),
                &worker.id().0,
                "epoch-stop",
                worker_version,
                "interrupt_expected",
                expected,
            )
            .await?;
        assert_eq!(replay.command.command_id, stopped.command.command_id);

        let deadline = Instant::now() + Duration::from_secs(4);
        while database
            .gateway_command(&stopped.command.command_id)?
            .is_some_and(|command| command.state == "accepted_by_source")
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            database
                .gateway_command(&stopped.command.command_id)?
                .context("stop command disappeared")?
                .state,
            "completed"
        );
        let origin: String = database.connect()?.query_row(
            "SELECT origin FROM gateway_commands WHERE command_id=?1",
            [&stopped.command.command_id],
            |row| row.get(0),
        )?;
        assert_eq!(origin, "worker_control");
        fake_server.await??;

        let persisted_worker = database
            .session_worker(&worker.id().0)?
            .context("stopped worker disappeared")?;
        assert_eq!(persisted_worker.state, "exited");
        assert_eq!(persisted_worker.error_code, None);

        let (connection_state, last_proxy_seq, close_reason): (String, i64, String) =
            database.connect()?.query_row(
                "SELECT state,last_proxy_seq,close_reason FROM worker_connection_epochs
                 WHERE worker_id=?1 ORDER BY opened_at_ms DESC LIMIT 1",
                [&worker.id().0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        assert_eq!(connection_state, "closed");
        assert!(last_proxy_seq > 0);
        assert_eq!(close_reason, "worker_stopping");
        assert!(
            database
                .thread_leases_for_worker(&worker.id().0)?
                .iter()
                .all(|lease| lease.state == "released")
        );
        assert!(
            database
                .terminal_attachments_for_worker(&worker.id().0)?
                .iter()
                .all(|attachment| matches!(attachment.state.as_str(), "closed" | "expired"))
        );
        assert!(database.active_input_lease(&worker.id().0)?.is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn real_session_path_uses_private_proxy_persistent_lease_and_single_worker() -> Result<()>
    {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::Builder::new()
            .prefix("session-real-")
            .tempdir_in("/private/tmp")?;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))?;
        let upstream_path = temp.path().join("app-server.sock");
        let listener = UnixListener::bind(&upstream_path)?;
        fs::set_permissions(&upstream_path, fs::Permissions::from_mode(0o600))?;
        let fake_server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut websocket = accept_async(stream).await?;
            while let Some(message) = websocket.next().await {
                let Ok(message) = message else {
                    break;
                };
                match message {
                    Message::Text(text) => {
                        let request: Value = serde_json::from_str(&text)?;
                        let Some(method) = request.get("method").and_then(Value::as_str) else {
                            continue;
                        };
                        match method {
                            "initialize" => {
                                websocket
                                    .send(Message::Text(
                                        json!({
                                            "id":request["id"],
                                            "result":{
                                                "codexHome":"/synthetic/codex-home",
                                                "userAgent":"codex_cli_rs/0.146.1-fixture"
                                            }
                                        })
                                        .to_string()
                                        .into(),
                                    ))
                                    .await?;
                            }
                            "thread/start" | "thread/resume" => {
                                websocket
                                    .send(Message::Text(
                                        json!({
                                            "id":request["id"],
                                            "result":{"thread":{"id":"thread-real-1","turns":[]}}
                                        })
                                        .to_string()
                                        .into(),
                                    ))
                                    .await?;
                            }
                            _ => {}
                        }
                    }
                    Message::Ping(payload) => websocket.send(Message::Pong(payload)).await?,
                    Message::Close(_) => break,
                    Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
                }
            }
            Result::<()>::Ok(())
        });

        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let writer = WriterHandle::start(database.clone(), 256, 128, 32)?;
        writer.upsert_source_kind(
            "source-real",
            "app_server",
            "fixture-upstream",
            &json!({}),
            "ready",
        )?;
        writer.record_live_capabilities("source-real", "epoch-real", &json!({}))?;
        let runtime = RuntimeRoot::prepare(temp.path().join("runtime"))?;
        let remote_cli = canonical_executable(&PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/fake_codex_remote.sh"
        )))?;
        let registry = SessionRegistry::new_runtime(
            runtime,
            None,
            Some(remote_cli),
            vec![SessionSource {
                source_id: "source-real".into(),
                store_source_id: "store-real".into(),
                codex_home: temp.path().join("codex-home"),
                upstream_socket: upstream_path,
            }],
            writer.clone(),
            database.clone(),
            [5; 32],
        );
        fs::create_dir(temp.path().join("codex-home"))?;
        let created = registry
            .create(
                "local_bearer".into(),
                "session-create-new-0001".into(),
                CreateSession {
                    source_id: "source-real".into(),
                    source_epoch: "epoch-real".into(),
                    expected_supervisor_version: 1,
                    mode: "new".into(),
                    codex_thread_id: None,
                    cwd: temp.path().to_path_buf(),
                    rows: 24,
                    cols: 80,
                },
            )
            .await?;
        let worker = registry
            .get(&created.snapshot.worker_id.0)
            .context("missing real worker")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while worker.snapshot().state != SessionWorkerState::Ready && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(worker.snapshot().state, SessionWorkerState::Ready);
        let leases = database.thread_leases_for_worker(&worker.id().0)?;
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].codex_thread_id.as_deref(), Some("thread-real-1"));
        assert_eq!(leases[0].state, "active");

        writer.upsert_turn_owner(TurnOwnerRecord {
            source_id: "source-real".into(),
            source_epoch: "epoch-real".into(),
            worker_id: worker.id().0.clone(),
            codex_thread_id: "thread-real-1".into(),
            codex_turn_id: "turn-active-1".into(),
            owner_type: "terminal".into(),
            owner_id: "attachment-fixture".into(),
            principal_id: "local_bearer".into(),
            input_lease_id: None,
            state: "active".into(),
            version: 1,
            start_command_id: None,
        })?;
        let worker_version = database
            .session_worker(&worker.id().0)?
            .context("missing persisted worker")?
            .version;
        let stop_conflict = registry
            .stop_session(
                "local_bearer".into(),
                "session-stop-active-rejected".into(),
                &worker.id().0,
                "epoch-real",
                worker_version,
                "reject_if_active",
                Vec::new(),
            )
            .await
            .expect_err("active Turn must block an unqualified stop");
        assert_eq!(stop_conflict.code, "TURN_STATE_CONFLICT");
        assert_eq!(worker.snapshot().state, SessionWorkerState::Ready);
        writer.complete_turn_owner(
            "source-real",
            "epoch-real",
            "thread-real-1",
            "turn-active-1",
            "completed",
            "fixture_completed",
            None,
        )?;

        let attachment = worker
            .prepare_attachment("local_bearer".into(), None, None)
            .await?;
        assert_eq!(
            database
                .terminal_attachment(&attachment.attachment_id)?
                .context("missing persisted attachment")?
                .state,
            "prepared"
        );
        let attachment_id = worker
            .connect_attachment("local_bearer".into(), attachment.descriptor)
            .await?;
        assert_eq!(
            database
                .terminal_attachment(&attachment_id)?
                .context("missing connected attachment")?
                .state,
            "connected"
        );
        let input = worker
            .acquire_input_lease(
                "local_bearer".into(),
                attachment_id.clone(),
                attachment.attachment_token.clone(),
                attachment.input_lease_version,
                false,
            )
            .await?;
        let persisted_input = database
            .active_input_lease(&worker.id().0)?
            .context("missing persisted input lease")?;
        assert_eq!(persisted_input.owner_id, attachment_id);
        assert_eq!(persisted_input.lease_id, input.lease_id.clone().unwrap());
        let released = worker
            .release_input_lease(
                "local_bearer".into(),
                persisted_input.owner_id,
                attachment.attachment_token,
                persisted_input.lease_id,
                input.version,
            )
            .await?;
        assert_eq!(released.version, 2);
        assert!(database.active_input_lease(&worker.id().0)?.is_none());
        assert_eq!(
            database
                .session_worker(&worker.id().0)?
                .context("missing persisted worker")?
                .input_lease_version,
            2
        );

        let first_resume = registry.create(
            "local_bearer".into(),
            "session-resume-existing-0002".into(),
            CreateSession {
                source_id: "source-real".into(),
                source_epoch: "epoch-real".into(),
                expected_supervisor_version: 1,
                mode: "resume".into(),
                codex_thread_id: Some("thread-real-1".into()),
                cwd: temp.path().to_path_buf(),
                rows: 24,
                cols: 80,
            },
        );
        let second_resume = registry.create(
            "local_cookie".into(),
            "session-resume-existing-0003".into(),
            CreateSession {
                source_id: "source-real".into(),
                source_epoch: "epoch-real".into(),
                expected_supervisor_version: 1,
                mode: "resume".into(),
                codex_thread_id: Some("thread-real-1".into()),
                cwd: temp.path().to_path_buf(),
                rows: 24,
                cols: 80,
            },
        );
        let (attached, attached_again) = tokio::join!(first_resume, second_resume);
        let attached = attached?;
        let attached_again = attached_again?;
        assert!(attached.existing_owner && attached_again.existing_owner);
        assert_eq!(attached.snapshot.worker_id, worker.snapshot().worker_id);
        assert_eq!(
            attached_again.snapshot.worker_id,
            worker.snapshot().worker_id
        );
        assert_eq!(registry.inner.workers.read().unwrap().len(), 1);
        assert_eq!(registry.inner.proxies.lock().unwrap().len(), 1);

        let stale_attachment = worker
            .prepare_attachment("local_bearer".into(), None, None)
            .await?;
        let stale_attachment_id = worker
            .connect_attachment("local_bearer".into(), stale_attachment.descriptor.clone())
            .await?;
        let stale_input = worker
            .acquire_input_lease(
                "local_bearer".into(),
                stale_attachment_id.clone(),
                stale_attachment.attachment_token,
                released.version,
                false,
            )
            .await?;
        assert!(stale_input.lease_id.is_some());

        registry.stale_source("source-real", "epoch-real").await;
        let deadline = Instant::now() + Duration::from_secs(2);
        while database
            .session_worker(&worker.id().0)?
            .is_some_and(|record| record.state != "stale_epoch")
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            database
                .session_worker(&worker.id().0)?
                .context("missing finalized worker")?
                .state,
            "stale_epoch"
        );
        assert_eq!(
            database.thread_leases_for_worker(&worker.id().0)?[0].state,
            "stale"
        );
        assert_eq!(
            database
                .terminal_attachment(&stale_attachment_id)?
                .context("missing finalized attachment")?
                .state,
            "orphaned"
        );
        assert!(database.active_input_lease(&worker.id().0)?.is_none());
        assert_eq!(worker.snapshot().state, SessionWorkerState::StaleEpoch);
        assert!(registry.inner.proxies.lock().unwrap().is_empty());
        fake_server.await??;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_worker_echo_resize_lease_and_stop_are_ordered() -> Result<()> {
        let (_temp, _registry, worker) = ready_worker().await?;
        assert_eq!(worker.snapshot().state, SessionWorkerState::Ready);
        let attachment = worker
            .prepare_attachment("principal".into(), None, None)
            .await?;
        let attachment_id = worker
            .connect_attachment("principal".into(), attachment.descriptor.clone())
            .await?;
        let lease = worker
            .acquire_input_lease(
                "principal".into(),
                attachment_id.clone(),
                attachment.attachment_token,
                attachment.input_lease_version,
                false,
            )
            .await?;
        let mut states = worker.subscribe_state();
        worker.resize(attachment_id.clone(), 30, 100).await?;
        assert!(states.has_changed()?);
        states.borrow_and_update();
        worker.resize(attachment_id.clone(), 30, 100).await?;
        assert!(
            !states.has_changed()?,
            "an identical terminal size must not publish another Worker state"
        );
        worker
            .write_input(
                attachment_id.clone(),
                lease.lease_id.clone().context("missing lease")?,
                b"hello\n".to_vec(),
            )
            .await?;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut snapshot = worker.terminal_snapshot(None).await?;
        let mut rendered = [snapshot.screen.clone(), snapshot.replay.clone()].concat();
        while !String::from_utf8_lossy(&rendered).contains("ECHO:hello")
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
            snapshot = worker.terminal_snapshot(None).await?;
            rendered = [snapshot.screen.clone(), snapshot.replay.clone()].concat();
        }
        assert!(String::from_utf8_lossy(&rendered).contains("ECHO:hello"));
        assert_eq!((snapshot.rows, snapshot.cols), (30, 100));
        assert_eq!(worker.stop().await?.state, SessionWorkerState::Stopping);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn descriptor_and_input_lease_are_single_owner() -> Result<()> {
        let (_temp, _registry, worker) = ready_worker().await?;
        let first = worker
            .prepare_attachment("principal".into(), None, None)
            .await?;
        let first_id = worker
            .connect_attachment("principal".into(), first.descriptor.clone())
            .await?;
        let reused = worker
            .connect_attachment("principal".into(), first.descriptor)
            .await;
        assert_eq!(reused.unwrap_err().code, "ATTACHMENT_DESCRIPTOR_INVALID");
        let lease = worker
            .acquire_input_lease(
                "principal".into(),
                first_id,
                first.attachment_token,
                first.input_lease_version,
                false,
            )
            .await?;

        let second = worker
            .prepare_attachment("principal".into(), None, None)
            .await?;
        let second_id = worker
            .connect_attachment("principal".into(), second.descriptor.clone())
            .await?;
        let forged = worker
            .acquire_input_lease(
                "principal".into(),
                second_id.clone(),
                "0".repeat(64),
                lease.version,
                false,
            )
            .await
            .unwrap_err();
        assert_eq!(forged.code, "ATTACHMENT_TOKEN_INVALID");
        let conflict = worker
            .acquire_input_lease(
                "principal".into(),
                second_id.clone(),
                second.attachment_token.clone(),
                lease.version,
                false,
            )
            .await
            .unwrap_err();
        assert_eq!(conflict.code, "INPUT_LEASE_CONFLICT");
        let unsafe_takeover = worker
            .acquire_input_lease(
                "principal".into(),
                second_id.clone(),
                second.attachment_token,
                lease.version,
                true,
            )
            .await
            .unwrap_err();
        assert_eq!(unsafe_takeover.code, "INPUT_LEASE_CONFLICT");
        let wrong_input = worker
            .write_input(second_id, "wrong".into(), b"unsafe".to_vec())
            .await
            .unwrap_err();
        assert_eq!(wrong_input.code, "INPUT_LEASE_REQUIRED");
        worker.stop().await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn attachment_control_token_guards_resume_acquire_and_release() -> Result<()> {
        let (_temp, _registry, worker) = ready_worker().await?;
        let attachment = worker
            .prepare_attachment("principal".into(), None, None)
            .await?;
        let attachment_id = worker
            .connect_attachment("principal".into(), attachment.descriptor)
            .await?;
        worker.disconnect_attachment(attachment_id.clone()).await;

        let missing_token = worker
            .prepare_attachment("principal".into(), Some(attachment_id.clone()), None)
            .await
            .unwrap_err();
        assert_eq!(missing_token.code, "ATTACHMENT_TOKEN_REQUIRED");
        let invalid_token = worker
            .prepare_attachment(
                "principal".into(),
                Some(attachment_id.clone()),
                Some("0".repeat(64)),
            )
            .await
            .unwrap_err();
        assert_eq!(invalid_token.code, "ATTACHMENT_TOKEN_INVALID");

        let resumed = worker
            .prepare_attachment(
                "principal".into(),
                Some(attachment_id.clone()),
                Some(attachment.attachment_token.clone()),
            )
            .await?;
        assert_eq!(resumed.attachment_id, attachment_id);
        assert_eq!(resumed.attachment_token, attachment.attachment_token);
        worker
            .connect_attachment("principal".into(), resumed.descriptor)
            .await?;

        let wrong_principal = worker
            .acquire_input_lease(
                "other-principal".into(),
                attachment_id.clone(),
                attachment.attachment_token.clone(),
                resumed.input_lease_version,
                false,
            )
            .await
            .unwrap_err();
        assert_eq!(wrong_principal.code, "ATTACHMENT_PRINCIPAL_MISMATCH");
        let lease = worker
            .acquire_input_lease(
                "principal".into(),
                attachment_id.clone(),
                attachment.attachment_token.clone(),
                resumed.input_lease_version,
                false,
            )
            .await?;
        let lease_id = lease.lease_id.clone().context("missing input lease")?;
        let forged_release = worker
            .release_input_lease(
                "principal".into(),
                attachment_id.clone(),
                "0".repeat(64),
                lease_id.clone(),
                lease.version,
            )
            .await
            .unwrap_err();
        assert_eq!(forged_release.code, "ATTACHMENT_TOKEN_INVALID");
        let released = worker
            .release_input_lease(
                "principal".into(),
                attachment_id,
                attachment.attachment_token,
                lease_id,
                lease.version,
            )
            .await?;
        assert_eq!(released.state, "none");
        for _ in 1..ATTACHMENT_CAPACITY {
            worker
                .prepare_attachment("principal".into(), None, None)
                .await?;
        }
        let capacity = worker
            .prepare_attachment("principal".into(), None, None)
            .await
            .unwrap_err();
        assert_eq!(capacity.code, "ATTACHMENT_LIMIT_REACHED");
        worker.stop().await?;
        Ok(())
    }

    #[test]
    fn expired_unowned_attachments_are_reclaimable_but_live_ones_are_retained() {
        let now = Instant::now();
        let mut attachment = Attachment {
            principal: "principal".into(),
            control_token_hash: blake3::hash(b"control-token"),
            descriptor_hash: None,
            descriptor_expires_at: now - Duration::from_millis(1),
            connected: false,
            disconnected_at: Some(now - DETACH_GRACE),
            last_ack: 0,
        };
        assert!(!retain_attachment("attachment", &attachment, None, now));
        assert!(retain_attachment(
            "attachment",
            &attachment,
            Some("attachment"),
            now
        ));
        attachment.connected = true;
        assert!(retain_attachment("attachment", &attachment, None, now));
        attachment.connected = false;
        attachment.descriptor_expires_at = now + Duration::from_millis(1);
        assert!(retain_attachment("attachment", &attachment, None, now));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_creation_is_idempotent_and_conflict_safe() -> Result<()> {
        let (temp, registry, worker) = ready_worker().await?;
        let replay = registry
            .create_fake(
                "principal".into(),
                "idempotency-key-0001".into(),
                CreateFakeSession {
                    cwd: temp.path().to_path_buf(),
                    rows: 24,
                    cols: 80,
                },
            )
            .await?;
        assert!(replay.replayed);
        assert_eq!(replay.snapshot.worker_id, *worker.id());
        let conflict = registry
            .create_fake(
                "principal".into(),
                "idempotency-key-0001".into(),
                CreateFakeSession {
                    cwd: temp.path().to_path_buf(),
                    rows: 25,
                    cols: 80,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(conflict.code, "IDEMPOTENCY_CONFLICT");
        worker.stop().await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_before_readiness_is_explicitly_not_ready() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new()?;
        let fixture = temp.path().join("fake-exits-before-ready");
        fs::write(&fixture, b"#!/bin/sh\nexit 0\n")?;
        fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700))?;
        let registry = SessionRegistry::new(
            RuntimeRoot::prepare(temp.path().join("runtime"))?,
            Some(fixture),
        );
        let created = registry
            .create_fake(
                "principal".into(),
                "early-exit-idempotency-key".into(),
                CreateFakeSession {
                    cwd: temp.path().to_path_buf(),
                    rows: 24,
                    cols: 80,
                },
            )
            .await?;
        let worker = registry
            .get(&created.snapshot.worker_id.0)
            .context("missing early-exit worker")?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !worker.snapshot().state.is_terminal() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let snapshot = worker.snapshot();
        assert_eq!(snapshot.state, SessionWorkerState::Exited);
        assert_eq!(
            snapshot.error_code.as_deref(),
            Some("SESSION_WORKER_NOT_READY")
        );
        assert!(snapshot.exit.is_some_and(|exit| exit.success));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protocol_connection_disarms_thread_readiness_timeout() -> Result<()> {
        let temp = tempfile::Builder::new()
            .prefix("session-protocol-wait-")
            .tempdir_in("/private/tmp")?;
        let executable = fake_cli(&temp)?;
        let runtime_root = RuntimeRoot::prepare(temp.path().join("runtime"))?;
        let worker_id = Uuid::new_v4().to_string();
        let runtime_dir = runtime_root.create_worker_dir(&worker_id)?;
        let worker = SessionWorkerHandle::spawn(
            SessionWorkerId(worker_id),
            SpawnSpec {
                executable,
                argv: Vec::new(),
                canonical_cwd: fs::canonicalize(temp.path())?,
                env_allowlist: safe_worker_environment(),
                rows: 24,
                cols: 80,
            },
            WorkerReadiness::AppServerProtocol,
            Duration::from_millis(100),
            runtime_root,
            runtime_dir,
            None,
        )?;
        let bridge = SessionProtocolBridge::default();
        bridge.connected();
        bridge.bind(worker.clone());

        tokio::time::sleep(Duration::from_millis(250)).await;
        let waiting = worker.snapshot();
        assert_eq!(waiting.state, SessionWorkerState::Connecting);
        assert_eq!(waiting.error_code, None);
        assert!(waiting.exit.is_none());

        worker.protocol_ready()?;
        let ready_deadline = Instant::now() + Duration::from_secs(1);
        while worker.snapshot().state != SessionWorkerState::Ready
            && Instant::now() < ready_deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(worker.snapshot().state, SessionWorkerState::Ready);

        worker.stop().await?;
        let exit_deadline = Instant::now() + Duration::from_secs(2);
        while !worker.snapshot().state.is_terminal() && Instant::now() < exit_deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(worker.snapshot().state.is_terminal());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_protocol_connection_still_times_out() -> Result<()> {
        let temp = tempfile::Builder::new()
            .prefix("session-protocol-timeout-")
            .tempdir_in("/private/tmp")?;
        let executable = fake_cli(&temp)?;
        let runtime_root = RuntimeRoot::prepare(temp.path().join("runtime"))?;
        let worker_id = Uuid::new_v4().to_string();
        let runtime_dir = runtime_root.create_worker_dir(&worker_id)?;
        let worker = SessionWorkerHandle::spawn(
            SessionWorkerId(worker_id),
            SpawnSpec {
                executable,
                argv: Vec::new(),
                canonical_cwd: fs::canonicalize(temp.path())?,
                env_allowlist: safe_worker_environment(),
                rows: 24,
                cols: 80,
            },
            WorkerReadiness::AppServerProtocol,
            Duration::from_millis(100),
            runtime_root,
            runtime_dir,
            None,
        )?;

        let deadline = Instant::now() + Duration::from_secs(2);
        while !worker.snapshot().state.is_terminal() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let timed_out = worker.snapshot();
        assert_eq!(timed_out.state, SessionWorkerState::Failed);
        assert_eq!(
            timed_out.error_code.as_deref(),
            Some("SESSION_WORKER_READINESS_TIMEOUT")
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn one_hundred_workers_stop_without_process_or_runtime_leaks() -> Result<()> {
        let temp = TempDir::new()?;
        let fixture = fake_cli(&temp)?;
        let runtime_path = temp.path().join("runtime");
        let registry =
            SessionRegistry::new(RuntimeRoot::prepare(runtime_path.clone())?, Some(fixture));
        let mut workers = Vec::new();
        for index in 0..100 {
            let created = registry
                .create_fake(
                    "stress-principal".into(),
                    format!("stress-worker-key-{index:04}"),
                    CreateFakeSession {
                        cwd: temp.path().to_path_buf(),
                        rows: 24,
                        cols: 80,
                    },
                )
                .await?;
            let worker = registry
                .get(&created.snapshot.worker_id.0)
                .context("missing stress worker")?;
            worker.stop().await?;
            workers.push(worker);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline
            && workers
                .iter()
                .any(|worker| !worker.snapshot().state.is_terminal())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            workers
                .iter()
                .all(|worker| worker.snapshot().state.is_terminal())
        );
        let cleanup_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let managed = fs::read_dir(&runtime_path)?
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("worker-"))
                .count();
            if managed == 0 {
                break;
            }
            if Instant::now() >= cleanup_deadline {
                anyhow::bail!("{managed} Session Worker runtime directories leaked");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ten_mib_output_remains_bounded_and_does_not_block_runtime() -> Result<()> {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new()?;
        let fixture = temp.path().join("fake-output-codex");
        fs::write(
            &fixture,
            b"#!/bin/sh\nprintf 'SESSION_READY\\r\\n'\n/bin/dd if=/dev/zero bs=1048576 count=10 2>/dev/null | /usr/bin/tr '\\0' x\n",
        )?;
        fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700))?;
        let registry = SessionRegistry::new(
            RuntimeRoot::prepare(temp.path().join("runtime"))?,
            Some(fixture),
        );
        let created = registry
            .create_fake(
                "output-principal".into(),
                "ten-mib-output-key".into(),
                CreateFakeSession {
                    cwd: temp.path().to_path_buf(),
                    rows: 24,
                    cols: 80,
                },
            )
            .await?;
        let worker = registry
            .get(&created.snapshot.worker_id.0)
            .context("missing output worker")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !worker.snapshot().state.is_terminal() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(worker.snapshot().state.is_terminal());
        assert!(worker.snapshot().output_seq > 100);
        let state = worker.snapshot();
        assert!(state.terminal_retained_bytes <= OUTPUT_JOURNAL_BYTES);
        assert!(state.terminal_checkpoint_bytes < 256 * 1024);
        Ok(())
    }
}
