use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::clock::now_ms;
use crate::domain::gateway::{
    GatewayCommandOrigin, GatewayCommandTarget, GatewayTransition, NewGatewayCommand,
    ReceiveGatewayCommand,
};
use crate::domain::identity::thread_key;
use crate::domain::model::{NormalizedEvent, OwnedIngestBatch};
use crate::domain::redact;
use crate::domain::session::{
    AcquireThreadLease, SessionWorkerRecord, SessionWorkerTransition, TurnOwnerRecord,
};
use crate::store::Database;
use crate::store::PendingRequestClaim;
use crate::writer::WriterHandle;

use super::{ProtocolClassification, ProxyDirection, ProxyOwner, ServerRequestRoute};
use crate::session::worker::SessionWorkerHandle;

#[derive(Debug, Clone)]
pub struct ProxyEnvelope {
    pub source_id: String,
    pub store_source_id: String,
    pub source_epoch: String,
    pub worker_id: String,
    pub connection_epoch: String,
    pub proxy_seq: u64,
    pub direction: ProxyDirection,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub struct MutationBoundary {
    pub command_id: String,
}

pub trait ProxyEventSink: Send + Sync + 'static {
    fn connection_opened(
        &self,
        _worker_id: &str,
        _source_id: &str,
        _source_epoch: &str,
        _connection_epoch: &str,
    ) -> Result<()> {
        Ok(())
    }

    fn commit_envelope(&self, envelope: &ProxyEnvelope) -> Result<()>;

    fn commit_decode_error(
        &self,
        envelope: &ProxyEnvelope,
        raw_text: &str,
        error_code: &'static str,
    ) -> Result<()>;

    fn server_request_route(&self, _envelope: &ProxyEnvelope) -> Result<ServerRequestRoute> {
        Ok(ServerRequestRoute::Terminal)
    }

    fn prepare_server_response(
        &self,
        _envelope: &ProxyEnvelope,
        _request_id: &str,
        _request_method: &str,
        _request_payload: &Value,
    ) -> Result<Option<MutationBoundary>> {
        Ok(None)
    }

    fn prepare_upstream_write(
        &self,
        envelope: &ProxyEnvelope,
        method: &str,
        classification: ProtocolClassification,
        owner: Option<&ProxyOwner>,
    ) -> Result<Option<MutationBoundary>>;

    fn upstream_write_completed(&self, boundary: &MutationBoundary) -> Result<()>;

    fn upstream_response_committed(
        &self,
        boundary: &MutationBoundary,
        response: &Value,
    ) -> Result<()>;

    fn outcome_unknown(&self, boundary: &MutationBoundary, reason: &'static str) -> Result<()>;

    fn protocol_anomaly(&self, _envelope: &ProxyEnvelope, _code: &'static str) -> Result<()> {
        Ok(())
    }

    fn connection_closed(
        &self,
        _worker_id: &str,
        _connection_epoch: &str,
        _last_proxy_seq: u64,
        _reason: &'static str,
    ) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct WriterProxyEventSink {
    writer: WriterHandle,
    fingerprint_key: [u8; 32],
}

#[derive(Default)]
struct SessionProtocolBridgeState {
    worker: Option<SessionWorkerHandle>,
    pending_connected: bool,
    pending_ready: bool,
    pending_failure: Option<&'static str>,
}

#[derive(Default)]
pub struct SessionProtocolBridge {
    state: Mutex<SessionProtocolBridgeState>,
}

impl SessionProtocolBridge {
    pub fn bind(&self, worker: SessionWorkerHandle) {
        let (pending_failure, pending_ready, pending_connected) = {
            let mut state = self.state.lock().expect("protocol bridge poisoned");
            state.worker = Some(worker.clone());
            (
                state.pending_failure.take(),
                std::mem::take(&mut state.pending_ready),
                std::mem::take(&mut state.pending_connected),
            )
        };
        if let Some(error_code) = pending_failure {
            let _ = worker.protocol_failed(error_code);
        } else if pending_ready {
            let _ = worker.protocol_ready();
        } else if pending_connected {
            let _ = worker.protocol_connected();
        }
    }

    pub(crate) fn connected(&self) {
        let worker = {
            let mut state = self.state.lock().expect("protocol bridge poisoned");
            if state.worker.is_none() {
                state.pending_connected = true;
            }
            state.worker.clone()
        };
        if let Some(worker) = worker {
            let _ = worker.protocol_connected();
        }
    }

    fn ready(&self) {
        let worker = {
            let mut state = self.state.lock().expect("protocol bridge poisoned");
            if state.worker.is_none() {
                state.pending_ready = true;
            }
            state.worker.clone()
        };
        if let Some(worker) = worker {
            let _ = worker.protocol_ready();
        }
    }

    fn failed(&self, error_code: &'static str) {
        let worker = {
            let mut state = self.state.lock().expect("protocol bridge poisoned");
            if state.worker.is_none() {
                state.pending_failure = Some(error_code);
            }
            state.worker.clone()
        };
        if let Some(worker) = worker {
            let _ = worker.protocol_failed(error_code);
        }
    }
}

pub struct SessionProxyEventSink {
    inner: WriterProxyEventSink,
    writer: WriterHandle,
    database: Arc<Database>,
    bridge: Arc<SessionProtocolBridge>,
    worker_id: String,
    primary_lease_id: String,
    create_command_id: String,
    primary_thread_id: Mutex<Option<String>>,
    initial_reservation_pending: Mutex<bool>,
    pending_lifecycle: Mutex<HashMap<String, PendingThreadLifecycle>>,
    pending_turn_starts: Mutex<HashMap<String, PendingTurnStart>>,
    ready: Mutex<bool>,
}

#[derive(Debug, Clone)]
enum PendingThreadLifecycle {
    Create { lease_id: String, role: String },
    Unsubscribe { thread_id: String },
}

#[derive(Debug, Clone)]
struct PendingTurnStart {
    thread_id: String,
    owner: ProxyOwner,
    owner_id: String,
    created_turn_id: Option<String>,
}

impl SessionProxyEventSink {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        writer: WriterHandle,
        database: Arc<Database>,
        fingerprint_key: [u8; 32],
        bridge: Arc<SessionProtocolBridge>,
        worker_id: String,
        primary_lease_id: String,
        create_command_id: String,
        primary_thread_id: Option<String>,
    ) -> Self {
        let initial_reservation_pending = primary_thread_id.is_none();
        Self {
            inner: WriterProxyEventSink::new(writer.clone(), fingerprint_key),
            writer,
            database,
            bridge,
            worker_id,
            primary_lease_id,
            create_command_id,
            primary_thread_id: Mutex::new(primary_thread_id),
            initial_reservation_pending: Mutex::new(initial_reservation_pending),
            pending_lifecycle: Mutex::new(HashMap::new()),
            pending_turn_starts: Mutex::new(HashMap::new()),
            ready: Mutex::new(false),
        }
    }

    fn observe_upstream_thread(&self, envelope: &ProxyEnvelope) -> Result<()> {
        if envelope.direction != ProxyDirection::UpstreamToTui {
            return Ok(());
        }
        if envelope.value.pointer("/result/thread/id").is_some()
            && !self
                .pending_lifecycle
                .lock()
                .expect("pending Thread lifecycle poisoned")
                .is_empty()
        {
            return Ok(());
        }
        let Some(thread_id) = extract_string(
            &envelope.value,
            &[
                "/params/threadId",
                "/params/thread/id",
                "/params/turn/threadId",
                "/result/thread/id",
                "/result/threadId",
            ],
        ) else {
            return Ok(());
        };
        let mut primary = self
            .primary_thread_id
            .lock()
            .expect("primary Thread state poisoned");
        if primary.is_none() {
            match self.writer.upgrade_thread_reservation(
                &self.primary_lease_id,
                &self.worker_id,
                1,
                &thread_id,
                Some(&self.create_command_id),
            )? {
                AcquireThreadLease::Acquired(_) | AcquireThreadLease::Existing(_) => {
                    *primary = Some(thread_id.clone());
                    *self
                        .initial_reservation_pending
                        .lock()
                        .expect("initial reservation state poisoned") = false;
                }
                AcquireThreadLease::ThreadOwned { .. } => {
                    self.bridge.failed("THREAD_ALREADY_OWNED");
                    anyhow::bail!("THREAD_ALREADY_OWNED");
                }
                AcquireThreadLease::VersionConflict => {
                    self.bridge.failed("THREAD_LEASE_CONFLICT");
                    anyhow::bail!("THREAD_LEASE_CONFLICT");
                }
            }
        } else if primary.as_deref() != Some(&thread_id) {
            match self.writer.acquire_thread_lease(
                &uuid::Uuid::new_v4().to_string(),
                &envelope.source_id,
                &envelope.source_epoch,
                &thread_id,
                &self.worker_id,
                "side",
                Some(&self.create_command_id),
            )? {
                AcquireThreadLease::Acquired(_) | AcquireThreadLease::Existing(_) => {}
                AcquireThreadLease::ThreadOwned { .. } => {
                    self.bridge.failed("THREAD_ALREADY_OWNED");
                    anyhow::bail!("THREAD_ALREADY_OWNED");
                }
                AcquireThreadLease::VersionConflict => {
                    self.bridge.failed("THREAD_LEASE_CONFLICT");
                    anyhow::bail!("THREAD_LEASE_CONFLICT");
                }
            }
        }
        let ready_thread_id = primary
            .clone()
            .context("Session Worker has no primary Thread after lease acquisition")?;
        drop(primary);
        self.mark_ready(&ready_thread_id)
    }

    fn request_owner(&self, envelope: &ProxyEnvelope) -> Result<Option<TurnOwnerRecord>> {
        let params = envelope.value.get("params").cloned().unwrap_or(Value::Null);
        let thread_id = extract_string(
            &params,
            &[
                "/threadId",
                "/thread/id",
                "/turn/threadId",
                "/item/threadId",
            ],
        );
        let turn_id = extract_string(&params, &["/turnId", "/turn/id", "/item/turnId"]);
        if let (Some(thread_id), Some(turn_id)) = (thread_id.as_deref(), turn_id.as_deref()) {
            return self.database.active_turn_owner(
                &envelope.source_id,
                &envelope.source_epoch,
                thread_id,
                turn_id,
            );
        }
        let mut candidates = self
            .database
            .active_turn_owners_for_worker(&self.worker_id)?
            .into_iter()
            .filter(|owner| {
                thread_id
                    .as_ref()
                    .is_none_or(|thread_id| owner.codex_thread_id == *thread_id)
            });
        let first = candidates.next();
        Ok(if candidates.next().is_some() {
            None
        } else {
            first
        })
    }

    fn prepare_thread_lifecycle(
        &self,
        envelope: &ProxyEnvelope,
        method: &str,
        boundary: Option<&MutationBoundary>,
    ) -> Result<()> {
        let command_id = boundary
            .map(|boundary| boundary.command_id.as_str())
            .unwrap_or(&self.create_command_id);
        let thread_id = extract_string(&envelope.value, &["/params/threadId", "/params/thread/id"]);

        if method == "thread/unsubscribe" {
            let Some(boundary) = boundary else {
                return Ok(());
            };
            let Some(thread_id) = thread_id else {
                return Ok(());
            };
            match self.database.active_thread_lease_owner(
                &envelope.source_id,
                &envelope.source_epoch,
                &thread_id,
            )? {
                Some(owner) if owner == self.worker_id => {
                    self.pending_lifecycle
                        .lock()
                        .expect("pending Thread lifecycle poisoned")
                        .insert(
                            boundary.command_id.clone(),
                            PendingThreadLifecycle::Unsubscribe { thread_id },
                        );
                }
                Some(_) => anyhow::bail!("THREAD_ALREADY_OWNED"),
                None => {
                    // Codex may issue a duplicate unsubscribe while clearing a session. The
                    // upstream operation is idempotent, and manufacturing a transient side
                    // lease here would corrupt the historical lease provenance.
                }
            }
            return Ok(());
        }

        if let Some(thread_id) = thread_id {
            match self.writer.acquire_thread_lease(
                &Uuid::new_v4().to_string(),
                &envelope.source_id,
                &envelope.source_epoch,
                &thread_id,
                &self.worker_id,
                "side",
                Some(command_id),
            )? {
                AcquireThreadLease::Acquired(_) | AcquireThreadLease::Existing(_) => {}
                AcquireThreadLease::ThreadOwned { .. } => anyhow::bail!("THREAD_ALREADY_OWNED"),
                AcquireThreadLease::VersionConflict => anyhow::bail!("THREAD_LEASE_CONFLICT"),
            }
        }

        let Some(boundary) = boundary else {
            return Ok(());
        };
        let pending = match method {
            "thread/start"
                if !*self
                    .initial_reservation_pending
                    .lock()
                    .expect("initial reservation state poisoned") =>
            {
                Some(self.reserve_protocol_thread(envelope, boundary, "primary")?)
            }
            "thread/fork" => Some(self.reserve_protocol_thread(envelope, boundary, "child")?),
            _ => None,
        };
        if let Some(pending) = pending {
            self.pending_lifecycle
                .lock()
                .expect("pending Thread lifecycle poisoned")
                .insert(boundary.command_id.clone(), pending);
        }
        Ok(())
    }

    fn reserve_protocol_thread(
        &self,
        envelope: &ProxyEnvelope,
        boundary: &MutationBoundary,
        role: &str,
    ) -> Result<PendingThreadLifecycle> {
        let lease_id = Uuid::new_v4().to_string();
        self.writer.reserve_thread_lease(
            &lease_id,
            &format!("protocol-{}", Uuid::new_v4()),
            &envelope.source_id,
            &envelope.source_epoch,
            &self.worker_id,
            role,
            Some(&boundary.command_id),
        )?;
        Ok(PendingThreadLifecycle::Create {
            lease_id,
            role: role.into(),
        })
    }

    fn complete_thread_lifecycle(
        &self,
        boundary: &MutationBoundary,
        response: &Value,
    ) -> Result<()> {
        let pending = self
            .pending_lifecycle
            .lock()
            .expect("pending Thread lifecycle poisoned")
            .remove(&boundary.command_id);
        let Some(pending) = pending else {
            return Ok(());
        };
        if response.get("error").is_some() {
            if let PendingThreadLifecycle::Create { lease_id, .. } = pending {
                self.writer.close_thread_reservation(
                    &lease_id,
                    &self.worker_id,
                    "released",
                    "upstream_rejected_thread_create",
                    Some(&boundary.command_id),
                )?;
            }
            return Ok(());
        }
        match pending {
            PendingThreadLifecycle::Create { lease_id, role } => {
                let thread_id =
                    extract_string(response, &["/result/thread/id", "/result/threadId"])
                        .context("thread lifecycle response omitted Thread ID")?;
                match self.writer.upgrade_thread_reservation(
                    &lease_id,
                    &self.worker_id,
                    1,
                    &thread_id,
                    Some(&boundary.command_id),
                )? {
                    AcquireThreadLease::Acquired(_) | AcquireThreadLease::Existing(_) => {
                        if role == "primary" {
                            *self
                                .primary_thread_id
                                .lock()
                                .expect("primary Thread state poisoned") = Some(thread_id);
                        }
                    }
                    AcquireThreadLease::ThreadOwned { .. } => {
                        self.bridge.failed("THREAD_ALREADY_OWNED");
                        anyhow::bail!("THREAD_ALREADY_OWNED");
                    }
                    AcquireThreadLease::VersionConflict => {
                        self.bridge.failed("THREAD_LEASE_CONFLICT");
                        anyhow::bail!("THREAD_LEASE_CONFLICT");
                    }
                }
            }
            PendingThreadLifecycle::Unsubscribe { thread_id } => {
                self.writer.release_thread_lease(
                    &self
                        .database
                        .session_worker(&self.worker_id)?
                        .context("Session Worker not found")?
                        .source_id,
                    &self
                        .database
                        .session_worker(&self.worker_id)?
                        .context("Session Worker not found")?
                        .source_epoch,
                    &thread_id,
                    &self.worker_id,
                    Some(&boundary.command_id),
                )?;
                let mut primary = self
                    .primary_thread_id
                    .lock()
                    .expect("primary Thread state poisoned");
                if primary.as_deref() == Some(&thread_id) {
                    *primary = None;
                }
            }
        }
        Ok(())
    }

    fn orphan_pending_thread_lifecycle(&self, boundary: &MutationBoundary) -> Result<()> {
        let pending = self
            .pending_lifecycle
            .lock()
            .expect("pending Thread lifecycle poisoned")
            .remove(&boundary.command_id);
        if let Some(PendingThreadLifecycle::Create { lease_id, .. }) = pending {
            self.writer.close_thread_reservation(
                &lease_id,
                &self.worker_id,
                "orphaned",
                "upstream_outcome_unknown",
                Some(&boundary.command_id),
            )?;
        }
        Ok(())
    }

    fn attributed_owner(
        &self,
        envelope: &ProxyEnvelope,
        method: &str,
        input_owner: Option<&ProxyOwner>,
    ) -> Result<Option<ProxyOwner>> {
        if matches!(method, "turn/steer" | "turn/interrupt")
            && let (Some(thread_id), Some(turn_id)) = (
                extract_string(&envelope.value, &["/params/threadId", "/params/thread/id"]),
                extract_string(
                    &envelope.value,
                    &[
                        "/params/expectedTurnId",
                        "/params/turnId",
                        "/params/turn/id",
                    ],
                ),
            )
            && let Some(owner) = self.database.active_turn_owner(
                &envelope.source_id,
                &envelope.source_epoch,
                &thread_id,
                &turn_id,
            )?
        {
            return Ok(Some(ProxyOwner {
                owner_type: owner.owner_type,
                owner_id: owner.owner_id,
                principal_id: owner.principal_id,
                input_lease_id: owner.input_lease_id,
            }));
        }
        Ok(input_owner.cloned())
    }

    fn prepare_turn_start(
        &self,
        envelope: &ProxyEnvelope,
        method: &str,
        boundary: Option<&MutationBoundary>,
        owner: Option<&ProxyOwner>,
    ) -> Result<()> {
        if method != "turn/start" {
            return Ok(());
        }
        let boundary = boundary.context("turn/start mutation boundary missing")?;
        let owner = owner.context("INPUT_ATTRIBUTION_UNKNOWN")?.clone();
        let thread_id = extract_string(&envelope.value, &["/params/threadId", "/params/thread/id"])
            .context("turn/start omitted Thread ID")?;
        let owner_id = owner.owner_id.clone();
        self.pending_turn_starts
            .lock()
            .expect("pending Turn start poisoned")
            .insert(
                boundary.command_id.clone(),
                PendingTurnStart {
                    thread_id,
                    owner,
                    owner_id,
                    created_turn_id: None,
                },
            );
        Ok(())
    }

    fn persist_turn_owner(
        &self,
        command_id: &str,
        pending: &PendingTurnStart,
        turn_id: &str,
    ) -> Result<()> {
        self.writer.upsert_turn_owner(TurnOwnerRecord {
            source_id: self
                .database
                .session_worker(&self.worker_id)?
                .context("Session Worker not found")?
                .source_id,
            source_epoch: self
                .database
                .session_worker(&self.worker_id)?
                .context("Session Worker not found")?
                .source_epoch,
            worker_id: self.worker_id.clone(),
            codex_thread_id: pending.thread_id.clone(),
            codex_turn_id: turn_id.into(),
            owner_type: pending.owner.owner_type.clone(),
            owner_id: pending.owner_id.clone(),
            principal_id: pending.owner.principal_id.clone(),
            input_lease_id: pending.owner.input_lease_id.clone(),
            state: "active".into(),
            version: 1,
            start_command_id: Some(command_id.into()),
        })?;
        Ok(())
    }

    fn observe_turn_event(&self, envelope: &ProxyEnvelope) -> Result<()> {
        if envelope.direction != ProxyDirection::UpstreamToTui {
            return Ok(());
        }
        let Some(method) = envelope.value.get("method").and_then(Value::as_str) else {
            return Ok(());
        };
        let Some(thread_id) = extract_string(
            &envelope.value,
            &[
                "/params/threadId",
                "/params/thread/id",
                "/params/turn/threadId",
            ],
        ) else {
            return Ok(());
        };
        let Some(turn_id) = extract_string(&envelope.value, &["/params/turn/id", "/params/turnId"])
        else {
            return Ok(());
        };
        if method == "turn/started" {
            let pending = {
                let mut starts = self
                    .pending_turn_starts
                    .lock()
                    .expect("pending Turn start poisoned");
                let Some((command_id, pending)) = starts.iter_mut().find(|(_, pending)| {
                    pending.thread_id == thread_id && pending.created_turn_id.is_none()
                }) else {
                    return Ok(());
                };
                pending.created_turn_id = Some(turn_id.clone());
                (command_id.clone(), pending.clone())
            };
            self.persist_turn_owner(&pending.0, &pending.1, &turn_id)?;
        } else if method == "turn/completed" {
            let status = envelope
                .value
                .pointer("/params/turn/status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let to_state = match status {
                "interrupted" | "cancelled" => "interrupted",
                "failed" => "failed",
                _ => "completed",
            };
            if self
                .database
                .active_turn_owner(
                    &envelope.source_id,
                    &envelope.source_epoch,
                    &thread_id,
                    &turn_id,
                )?
                .is_some()
            {
                self.writer.complete_turn_owner(
                    &envelope.source_id,
                    &envelope.source_epoch,
                    &thread_id,
                    &turn_id,
                    to_state,
                    "turn_completed_notification",
                    None,
                )?;
            }
        }
        Ok(())
    }

    fn complete_turn_start(&self, boundary: &MutationBoundary, response: &Value) -> Result<()> {
        let pending = self
            .pending_turn_starts
            .lock()
            .expect("pending Turn start poisoned")
            .remove(&boundary.command_id);
        let Some(pending) = pending else {
            return Ok(());
        };
        if response.get("error").is_some() || pending.created_turn_id.is_some() {
            return Ok(());
        }
        let turn_id = extract_string(response, &["/result/turn/id", "/result/turnId"])
            .context("turn/start response omitted Turn ID")?;
        self.persist_turn_owner(&boundary.command_id, &pending, &turn_id)
    }

    fn mark_ready(&self, thread_id: &str) -> Result<()> {
        let mut ready = self.ready.lock().expect("proxy readiness poisoned");
        if *ready {
            return Ok(());
        }
        let worker = self
            .database
            .session_worker(&self.worker_id)?
            .context("Session Worker disappeared while becoming ready")?;
        if worker.state != "ready" {
            self.writer
                .transition_session_worker(SessionWorkerTransition {
                    worker_id: self.worker_id.clone(),
                    expected_version: worker.version,
                    to_state: "ready".into(),
                    pid: None,
                    primary_thread_id: Some(thread_id.into()),
                    error_code: None,
                    reason_code: Some("thread_ready".into()),
                    command_id: Some(self.create_command_id.clone()),
                })?;
        }
        transition_command(
            &self.writer,
            &self.create_command_id,
            "completed",
            None,
            Some(json!({"workerId":self.worker_id,"threadId":thread_id}).to_string()),
        )?;
        *ready = true;
        self.bridge.ready();
        Ok(())
    }

    fn mark_connection_state(&self, to_state: &str, reason: &str) -> Result<SessionWorkerRecord> {
        let worker = self
            .database
            .session_worker(&self.worker_id)?
            .context("Session Worker not found")?;
        self.writer
            .transition_session_worker(SessionWorkerTransition {
                worker_id: self.worker_id.clone(),
                expected_version: worker.version,
                to_state: to_state.into(),
                pid: None,
                primary_thread_id: None,
                error_code: (to_state == "failed").then(|| reason.into()),
                reason_code: Some(reason.into()),
                command_id: Some(self.create_command_id.clone()),
            })
    }
}

impl WriterProxyEventSink {
    pub fn new(writer: WriterHandle, fingerprint_key: [u8; 32]) -> Self {
        Self {
            writer,
            fingerprint_key,
        }
    }

    fn transition(
        &self,
        command_id: &str,
        to_state: &str,
        error_code: Option<&str>,
        decision: &str,
        outcome: &str,
    ) -> Result<()> {
        self.writer.transition_gateway_command(GatewayTransition {
            command_id: command_id.into(),
            to_state: to_state.into(),
            result_summary_json: None,
            error_code: error_code.map(str::to_string),
            error_message: error_code
                .map(|_| "protocol operation did not reach a confirmed result".into()),
            reason_code: error_code.map(str::to_string),
            decision: decision.into(),
            outcome: outcome.into(),
        })?;
        Ok(())
    }
}

impl ProxyEventSink for WriterProxyEventSink {
    fn connection_opened(
        &self,
        worker_id: &str,
        source_id: &str,
        source_epoch: &str,
        connection_epoch: &str,
    ) -> Result<()> {
        self.writer
            .open_worker_connection(worker_id, source_id, source_epoch, connection_epoch)
    }

    fn commit_envelope(&self, envelope: &ProxyEnvelope) -> Result<()> {
        let original = serde_json::to_vec(&envelope.value)?;
        let source_fingerprint = blake3::keyed_hash(&self.fingerprint_key, &original)
            .to_hex()
            .to_string();
        let (redacted, redaction_audit) = redact::redact(&envelope.value, &self.fingerprint_key);
        let stored = serde_json::to_string(&redacted)?;
        let method = redacted
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("app_server/response")
            .to_string();
        let params = redacted
            .get("params")
            .or_else(|| redacted.get("result"))
            .cloned()
            .unwrap_or(Value::Null);
        let codex_thread_id = extract_string(
            &params,
            &[
                "/threadId",
                "/thread/id",
                "/turn/threadId",
                "/item/threadId",
            ],
        )
        .unwrap_or_default();
        let turn_id = extract_string(&params, &["/turnId", "/turn/id", "/item/turnId"]);
        let request = redacted.get("method").is_some() && redacted.get("id").is_some();
        let request_id = request
            .then(|| redacted.get("id"))
            .flatten()
            .map(|id| serde_json::to_string(id).expect("JSON id serialization cannot fail"));
        let phase = if request {
            "request"
        } else if method.ends_with("/started") || method.ends_with("/start") {
            "started"
        } else if method.ends_with("/completed") || method.ends_with("/complete") {
            "completed"
        } else if method.ends_with("/delta") || method.contains("/updated") {
            "delta"
        } else {
            "snapshot"
        };
        let observer_thread_key = if codex_thread_id.is_empty() {
            String::new()
        } else {
            thread_key(&envelope.store_source_id, &codex_thread_id)
        };
        let direction = envelope.direction.as_str().to_string();
        let event = NormalizedEvent {
            event_id: Uuid::now_v7().to_string(),
            source_id: envelope.source_id.clone(),
            store_source_id: envelope.store_source_id.clone(),
            epoch_id: envelope.source_epoch.clone(),
            source_seq: i64::try_from(envelope.proxy_seq).context("proxy sequence overflow")?,
            dedupe_key: format!(
                "proxy:{}:{}:{}",
                envelope.worker_id, envelope.connection_epoch, envelope.proxy_seq
            ),
            observed_at_ms: now_ms(),
            event_at_ms: None,
            thread_key: observer_thread_key,
            codex_thread_id: codex_thread_id.clone(),
            turn_id,
            item_id: None,
            request_id,
            blob_id: None,
            protocol_direction: Some(direction),
            worker_id: Some(envelope.worker_id.clone()),
            worker_connection_epoch: Some(envelope.connection_epoch.clone()),
            proxy_seq: Some(i64::try_from(envelope.proxy_seq)?),
            method,
            phase: phase.into(),
            durability: "transient".into(),
            projectable: !codex_thread_id.is_empty(),
            source_fingerprint,
            stored_raw_hash: blake3::hash(stored.as_bytes()).to_hex().to_string(),
            raw_json: stored,
            redaction_json: redaction_audit.to_string(),
            decode_status: "decoded".into(),
            decode_error: None,
            top_type: "app_server_proxy".into(),
            item_type: None,
            item_status: None,
            summary_text: None,
            payload: params,
        };
        self.writer.ingest(OwnedIngestBatch {
            source_id: envelope.source_id.clone(),
            epoch_id: envelope.source_epoch.clone(),
            checkpoint_key: format!("proxy:{}:{}", envelope.worker_id, envelope.connection_epoch),
            file_identity: format!("worker:{}", envelope.worker_id),
            byte_offset: 0,
            ordinal: envelope.proxy_seq,
            current_turn_id: event.turn_id.clone(),
            clean_eof: false,
            events: vec![event],
        })?;
        Ok(())
    }

    fn commit_decode_error(
        &self,
        envelope: &ProxyEnvelope,
        raw_text: &str,
        error_code: &'static str,
    ) -> Result<()> {
        let source_fingerprint = blake3::keyed_hash(&self.fingerprint_key, raw_text.as_bytes())
            .to_hex()
            .to_string();
        let (redacted, redaction_audit) =
            redact::redact(&Value::String(raw_text.to_string()), &self.fingerprint_key);
        let stored = serde_json::to_string(&redacted)?;
        self.writer.ingest(OwnedIngestBatch {
            source_id: envelope.source_id.clone(),
            epoch_id: envelope.source_epoch.clone(),
            checkpoint_key: format!("proxy:{}:{}", envelope.worker_id, envelope.connection_epoch),
            file_identity: format!("worker:{}", envelope.worker_id),
            byte_offset: 0,
            ordinal: envelope.proxy_seq,
            current_turn_id: None,
            clean_eof: false,
            events: vec![NormalizedEvent {
                event_id: Uuid::now_v7().to_string(),
                source_id: envelope.source_id.clone(),
                store_source_id: envelope.store_source_id.clone(),
                epoch_id: envelope.source_epoch.clone(),
                source_seq: i64::try_from(envelope.proxy_seq).context("proxy sequence overflow")?,
                dedupe_key: format!(
                    "proxy:{}:{}:{}",
                    envelope.worker_id, envelope.connection_epoch, envelope.proxy_seq
                ),
                observed_at_ms: now_ms(),
                event_at_ms: None,
                thread_key: String::new(),
                codex_thread_id: String::new(),
                turn_id: None,
                item_id: None,
                request_id: None,
                blob_id: None,
                protocol_direction: Some(envelope.direction.as_str().to_string()),
                worker_id: Some(envelope.worker_id.clone()),
                worker_connection_epoch: Some(envelope.connection_epoch.clone()),
                proxy_seq: Some(i64::try_from(envelope.proxy_seq)?),
                method: "app_server/decode_error".into(),
                phase: "error".into(),
                durability: "transient".into(),
                projectable: false,
                source_fingerprint,
                stored_raw_hash: blake3::hash(stored.as_bytes()).to_hex().to_string(),
                raw_json: stored,
                redaction_json: redaction_audit.to_string(),
                decode_status: "error".into(),
                decode_error: Some(error_code.into()),
                top_type: "app_server_proxy".into(),
                item_type: None,
                item_status: None,
                summary_text: None,
                payload: Value::Null,
            }],
        })?;
        Ok(())
    }

    fn prepare_upstream_write(
        &self,
        envelope: &ProxyEnvelope,
        method: &str,
        classification: ProtocolClassification,
        owner: Option<&ProxyOwner>,
    ) -> Result<Option<MutationBoundary>> {
        if classification == ProtocolClassification::KnownReadOnly {
            return Ok(None);
        }
        let owner = owner.context("INPUT_ATTRIBUTION_UNKNOWN")?;
        let command_id = Uuid::now_v7().to_string();
        let payload_hash = blake3::hash(serde_json::to_string(&envelope.value)?.as_bytes())
            .to_hex()
            .to_string();
        let params = envelope.value.get("params").cloned().unwrap_or(Value::Null);
        let codex_thread_id = extract_string(
            &params,
            &[
                "/threadId",
                "/thread/id",
                "/turn/threadId",
                "/item/threadId",
            ],
        );
        let codex_turn_id = extract_string(&params, &["/turnId", "/turn/id", "/item/turnId"]);
        let received = self.writer.receive_gateway_command(NewGatewayCommand {
            command_id: command_id.clone(),
            principal_id: owner.principal_id.clone(),
            capability: format!("tui.protocol.{method}"),
            idempotency_key: format!(
                "proxy:{}:{}:{}",
                envelope.worker_id, envelope.connection_epoch, envelope.proxy_seq
            ),
            payload_hash,
            target: GatewayCommandTarget {
                source_id: envelope.source_id.clone(),
                source_epoch: envelope.source_epoch.clone(),
                thread_key: codex_thread_id
                    .as_deref()
                    .map(|id| thread_key(&envelope.store_source_id, id)),
                codex_thread_id,
                expected_turn_id: codex_turn_id,
                expected_request_id: None,
                expected_request_version: None,
            },
            input_summary_json: json!({
                "method": method,
                "workerId": envelope.worker_id,
                "connectionEpoch": envelope.connection_epoch,
                "proxySeq": envelope.proxy_seq,
                "inputLeaseId": owner.input_lease_id,
                "classification": classification.as_str(),
            })
            .to_string(),
            origin: GatewayCommandOrigin::Tui,
        })?;
        match received {
            ReceiveGatewayCommand::Created(_) | ReceiveGatewayCommand::Existing(_) => {}
            ReceiveGatewayCommand::Conflict => anyhow::bail!("proxy idempotency conflict"),
        }
        self.transition(&command_id, "authorized", None, "allow", "authorized")?;
        self.transition(&command_id, "dispatching", None, "allow", "dispatching")?;
        Ok(Some(MutationBoundary { command_id }))
    }

    fn upstream_write_completed(&self, boundary: &MutationBoundary) -> Result<()> {
        self.transition(
            &boundary.command_id,
            "accepted_by_source",
            None,
            "allow",
            "written_upstream",
        )
    }

    fn upstream_response_committed(
        &self,
        boundary: &MutationBoundary,
        response: &Value,
    ) -> Result<()> {
        if response.get("error").is_some() {
            self.transition(
                &boundary.command_id,
                "failed",
                Some("UPSTREAM_REJECTED"),
                "observe",
                "failed",
            )
        } else {
            self.transition(
                &boundary.command_id,
                "completed",
                None,
                "observe",
                "completed",
            )
        }
    }

    fn outcome_unknown(&self, boundary: &MutationBoundary, reason: &'static str) -> Result<()> {
        self.transition(
            &boundary.command_id,
            "outcome_unknown",
            Some(reason),
            "observe",
            "outcome_unknown",
        )
    }

    fn connection_closed(
        &self,
        worker_id: &str,
        connection_epoch: &str,
        last_proxy_seq: u64,
        reason: &'static str,
    ) -> Result<()> {
        self.writer
            .close_worker_connection(worker_id, connection_epoch, last_proxy_seq, reason)
    }
}

impl ProxyEventSink for SessionProxyEventSink {
    fn connection_opened(
        &self,
        worker_id: &str,
        source_id: &str,
        source_epoch: &str,
        connection_epoch: &str,
    ) -> Result<()> {
        self.inner
            .connection_opened(worker_id, source_id, source_epoch, connection_epoch)?;
        self.mark_connection_state("connecting", "proxy_connected")?;
        transition_command(
            &self.writer,
            &self.create_command_id,
            "accepted_by_source",
            None,
            None,
        )?;
        self.bridge.connected();
        Ok(())
    }

    fn commit_envelope(&self, envelope: &ProxyEnvelope) -> Result<()> {
        self.inner.commit_envelope(envelope)?;
        self.observe_upstream_thread(envelope)?;
        self.observe_turn_event(envelope)
    }

    fn commit_decode_error(
        &self,
        envelope: &ProxyEnvelope,
        raw_text: &str,
        error_code: &'static str,
    ) -> Result<()> {
        self.inner
            .commit_decode_error(envelope, raw_text, error_code)
    }

    fn server_request_route(&self, envelope: &ProxyEnvelope) -> Result<ServerRequestRoute> {
        let owner = self.request_owner(envelope)?;
        let Some(owner) = owner else {
            return Ok(ServerRequestRoute::Unattributed);
        };
        if owner.owner_type == "terminal" {
            Ok(ServerRequestRoute::Terminal)
        } else if matches!(owner.owner_type.as_str(), "channel" | "gateway") {
            Ok(ServerRequestRoute::Channel {
                owner_type: owner.owner_type,
                owner_id: owner.owner_id,
                principal_id: owner.principal_id,
            })
        } else {
            Ok(ServerRequestRoute::Unattributed)
        }
    }

    fn prepare_server_response(
        &self,
        envelope: &ProxyEnvelope,
        request_id: &str,
        request_method: &str,
        request_payload: &Value,
    ) -> Result<Option<MutationBoundary>> {
        let request_envelope = ProxyEnvelope {
            direction: ProxyDirection::UpstreamToTui,
            value: json!({"method":request_method,"id":Value::Null,"params":request_payload}),
            ..envelope.clone()
        };
        let owner = self
            .request_owner(&request_envelope)?
            .context("REQUEST_OWNER_UNATTRIBUTED")?;
        if owner.owner_type != "terminal" {
            anyhow::bail!("REQUEST_OWNED_BY_CHANNEL");
        }
        let pending = self
            .database
            .pending_request_target(&envelope.source_id, &envelope.source_epoch, request_id)?
            .context("REQUEST_NOT_PENDING")?;
        if pending.state != "pending" {
            anyhow::bail!("REQUEST_NOT_PENDING");
        }
        let command_id = Uuid::now_v7().to_string();
        let payload_hash = blake3::hash(serde_json::to_string(&envelope.value)?.as_bytes())
            .to_hex()
            .to_string();
        let expected_turn_id =
            extract_string(request_payload, &["/turnId", "/turn/id", "/item/turnId"]);
        let received = self.writer.receive_gateway_command(NewGatewayCommand {
            command_id: command_id.clone(),
            principal_id: owner.principal_id,
            capability: format!("tui.protocol.{request_method}.response"),
            idempotency_key: format!(
                "proxy-response:{}:{}:{}",
                envelope.worker_id, envelope.connection_epoch, envelope.proxy_seq
            ),
            payload_hash,
            target: GatewayCommandTarget {
                source_id: envelope.source_id.clone(),
                source_epoch: envelope.source_epoch.clone(),
                thread_key: pending.thread_key,
                codex_thread_id: pending.codex_thread_id,
                expected_turn_id,
                expected_request_id: Some(request_id.into()),
                expected_request_version: Some(pending.request_version),
            },
            input_summary_json: json!({
                "method": request_method,
                "workerId": envelope.worker_id,
                "connectionEpoch": envelope.connection_epoch,
                "proxySeq": envelope.proxy_seq,
                "requestType": pending.request_type,
            })
            .to_string(),
            origin: GatewayCommandOrigin::Tui,
        })?;
        match received {
            ReceiveGatewayCommand::Created(_) | ReceiveGatewayCommand::Existing(_) => {}
            ReceiveGatewayCommand::Conflict => anyhow::bail!("proxy idempotency conflict"),
        }
        transition_command(&self.writer, &command_id, "authorized", None, None)?;
        match self.writer.claim_pending_request(&command_id)? {
            PendingRequestClaim::Claimed(_) => Ok(Some(MutationBoundary { command_id })),
            PendingRequestClaim::AlreadyResolved => anyhow::bail!("REQUEST_ALREADY_RESOLVED"),
            PendingRequestClaim::SourceEpochStale => anyhow::bail!("SOURCE_EPOCH_STALE"),
            PendingRequestClaim::NotPending => anyhow::bail!("REQUEST_NOT_PENDING"),
        }
    }

    fn prepare_upstream_write(
        &self,
        envelope: &ProxyEnvelope,
        method: &str,
        classification: ProtocolClassification,
        owner: Option<&ProxyOwner>,
    ) -> Result<Option<MutationBoundary>> {
        let attributed_owner = self.attributed_owner(envelope, method, owner)?;
        let boundary = self.inner.prepare_upstream_write(
            envelope,
            method,
            classification,
            attributed_owner.as_ref(),
        )?;
        let preparation = self
            .prepare_turn_start(
                envelope,
                method,
                boundary.as_ref(),
                attributed_owner.as_ref(),
            )
            .and_then(|()| self.prepare_thread_lifecycle(envelope, method, boundary.as_ref()));
        if let Err(error) = preparation {
            if let Some(boundary) = boundary.as_ref() {
                let _ = self.inner.transition(
                    &boundary.command_id,
                    "failed",
                    Some("SESSION_PROTOCOL_PRECONDITION_FAILED"),
                    "deny",
                    "failed",
                );
            }
            self.bridge.failed("SESSION_PROTOCOL_PRECONDITION_FAILED");
            return Err(error);
        }
        Ok(boundary)
    }

    fn upstream_write_completed(&self, boundary: &MutationBoundary) -> Result<()> {
        self.inner.upstream_write_completed(boundary)
    }

    fn upstream_response_committed(
        &self,
        boundary: &MutationBoundary,
        response: &Value,
    ) -> Result<()> {
        self.complete_thread_lifecycle(boundary, response)?;
        self.complete_turn_start(boundary, response)?;
        if let Some(request_id) = response
            .pointer("/result/requestId")
            .and_then(Value::as_str)
        {
            self.writer
                .complete_pending_request_action(&boundary.command_id, request_id)?;
            return Ok(());
        }
        self.inner.upstream_response_committed(boundary, response)
    }

    fn outcome_unknown(&self, boundary: &MutationBoundary, reason: &'static str) -> Result<()> {
        self.orphan_pending_thread_lifecycle(boundary)?;
        self.pending_turn_starts
            .lock()
            .expect("pending Turn start poisoned")
            .remove(&boundary.command_id);
        self.inner.outcome_unknown(boundary, reason)
    }

    fn protocol_anomaly(&self, envelope: &ProxyEnvelope, code: &'static str) -> Result<()> {
        self.inner.protocol_anomaly(envelope, code)
    }

    fn connection_closed(
        &self,
        worker_id: &str,
        connection_epoch: &str,
        last_proxy_seq: u64,
        reason: &'static str,
    ) -> Result<()> {
        let worker_before_close = self.database.session_worker(&self.worker_id)?;
        let reason = if worker_before_close
            .as_ref()
            .is_some_and(|worker| matches!(worker.state.as_str(), "stopping" | "exited"))
        {
            "worker_stopping"
        } else {
            reason
        };
        self.inner
            .connection_closed(worker_id, connection_epoch, last_proxy_seq, reason)?;
        let was_ready = *self.ready.lock().expect("proxy readiness poisoned");
        let current = self.database.session_worker(&self.worker_id)?;
        if current.as_ref().is_some_and(|worker| {
            !matches!(
                worker.state.as_str(),
                "stopping" | "exited" | "failed" | "stale_epoch" | "orphaned"
            )
        }) {
            let _ = self.mark_connection_state("failed", "SESSION_PROXY_DISCONNECTED");
        }
        if !was_ready {
            if reason == "worker_stopping" {
                let _ = transition_command(
                    &self.writer,
                    &self.create_command_id,
                    "cancelled",
                    None,
                    Some(
                        json!({"workerId":self.worker_id,"state":"stopped_before_ready"})
                            .to_string(),
                    ),
                );
            } else {
                let _ = transition_command(
                    &self.writer,
                    &self.create_command_id,
                    "outcome_unknown",
                    Some("SESSION_PROXY_DISCONNECTED"),
                    None,
                );
            }
        }
        if reason != "worker_stopping" {
            self.bridge.failed("SESSION_PROXY_DISCONNECTED");
        }
        Ok(())
    }
}

fn transition_command(
    writer: &WriterHandle,
    command_id: &str,
    to_state: &str,
    error_code: Option<&str>,
    result_summary_json: Option<String>,
) -> Result<()> {
    writer.transition_gateway_command(GatewayTransition {
        command_id: command_id.into(),
        to_state: to_state.into(),
        result_summary_json,
        error_code: error_code.map(str::to_string),
        error_message: error_code
            .map(|_| "Session Worker protocol outcome is not confirmed".into()),
        reason_code: error_code.map(str::to_string),
        decision: if error_code.is_some() {
            "deny"
        } else {
            "allow"
        }
        .into(),
        outcome: if error_code.is_some() {
            "outcome_unknown"
        } else {
            to_state
        }
        .into(),
    })?;
    Ok(())
}

fn extract_string(value: &Value, pointers: &[&str]) -> Option<String> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
        .map(str::to_string)
}

#[cfg(test)]
mod writer_tests {
    use std::sync::Arc;

    use super::*;
    use crate::domain::gateway::{GatewayCommandOrigin, GatewayCommandTarget, NewGatewayCommand};
    use crate::domain::session::{RegisterSessionWorker, SessionWorkerRegistration};
    use crate::store::Database;
    use tempfile::TempDir;

    #[test]
    fn writer_sink_persists_proxy_provenance_and_tui_write_boundary_without_body() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let writer = WriterHandle::start(database.clone(), 128, 128, 16)?;
        writer.upsert_source_kind(
            "source-1",
            "app_server",
            "fixture-upstream",
            &json!({}),
            "ready",
        )?;
        writer.record_live_capabilities("source-1", "source-epoch-1", &json!({}))?;
        let sink = WriterProxyEventSink::new(writer, [9_u8; 32]);
        sink.connection_opened("worker-1", "source-1", "source-epoch-1", "connection-1")?;
        let envelope = ProxyEnvelope {
            source_id: "source-1".into(),
            store_source_id: "store-1".into(),
            source_epoch: "source-epoch-1".into(),
            worker_id: "worker-1".into(),
            connection_epoch: "connection-1".into(),
            proxy_seq: 1,
            direction: ProxyDirection::TuiToUpstream,
            value: json!({
                "method":"turn/start",
                "id":7,
                "params":{"threadId":"thread-1","input":[{"type":"text","text":"private body"}]}
            }),
        };
        sink.commit_envelope(&envelope)?;
        let boundary = sink
            .prepare_upstream_write(
                &envelope,
                "turn/start",
                ProtocolClassification::KnownMutation,
                Some(&ProxyOwner {
                    owner_type: "terminal".into(),
                    owner_id: "attachment-1".into(),
                    principal_id: "local_bearer".into(),
                    input_lease_id: Some("input-lease-1".into()),
                }),
            )?
            .context("mutation boundary missing")?;
        sink.upstream_write_completed(&boundary)?;
        sink.upstream_response_committed(&boundary, &json!({"id":7,"result":{}}))?;
        sink.connection_closed("worker-1", "connection-1", 2, "connection_closed")?;

        let connection = database.connect()?;
        let provenance: (String, String, String, i64) = connection.query_row(
            "SELECT protocol_direction,worker_id,worker_connection_epoch,proxy_seq
             FROM raw_events WHERE dedupe_key='proxy:worker-1:connection-1:1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(
            provenance,
            (
                "tui_to_upstream".into(),
                "worker-1".into(),
                "connection-1".into(),
                1,
            )
        );
        let command: (String, String, String) = connection.query_row(
            "SELECT origin,state,input_summary_json FROM gateway_commands WHERE command_id=?1",
            [&boundary.command_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(command.0, "tui");
        assert_eq!(command.1, "completed");
        assert!(!command.2.contains("private body"));
        let audit_text: String = connection.query_row(
            "SELECT group_concat(input_summary_json,'') FROM control_audit WHERE command_id=?1",
            [&boundary.command_id],
            |row| row.get(0),
        )?;
        assert!(!audit_text.contains("private body"));
        let connection_state: (String, i64, String) = connection.query_row(
            "SELECT state,last_proxy_seq,close_reason FROM worker_connection_epochs
             WHERE worker_id='worker-1' AND connection_epoch='connection-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            connection_state,
            ("closed".into(), 2, "connection_closed".into())
        );
        Ok(())
    }

    #[test]
    fn tui_protocol_crash_boundary_reopen_matrix_never_replays_or_invents_completion() -> Result<()>
    {
        #[derive(Clone, Copy)]
        enum CrashAfter {
            BeforeRaw,
            Raw,
            Dispatch,
            WriteComplete,
            Response,
        }

        for (case, crash_after, expected_raw, expected_state) in [
            ("before-raw", CrashAfter::BeforeRaw, 0, None),
            ("after-raw", CrashAfter::Raw, 1, None),
            (
                "after-dispatch",
                CrashAfter::Dispatch,
                1,
                Some("outcome_unknown"),
            ),
            (
                "after-write-complete",
                CrashAfter::WriteComplete,
                1,
                Some("outcome_unknown"),
            ),
            ("after-response", CrashAfter::Response, 1, Some("completed")),
        ] {
            let temp = TempDir::new()?;
            let path = temp.path().join("observer.sqlite");
            let database = Arc::new(Database::open(&path)?);
            database.migrate()?;
            let writer = WriterHandle::start(database.clone(), 128, 128, 16)?;
            writer.upsert_source_kind(
                "source-crash",
                "app_server",
                "fixture-upstream",
                &json!({}),
                "ready",
            )?;
            writer.record_live_capabilities("source-crash", "source-epoch-crash", &json!({}))?;
            let sink = WriterProxyEventSink::new(writer.clone(), [4_u8; 32]);
            sink.connection_opened(
                "worker-crash",
                "source-crash",
                "source-epoch-crash",
                "connection-crash",
            )?;
            let envelope = ProxyEnvelope {
                source_id: "source-crash".into(),
                store_source_id: "store-crash".into(),
                source_epoch: "source-epoch-crash".into(),
                worker_id: "worker-crash".into(),
                connection_epoch: "connection-crash".into(),
                proxy_seq: 1,
                direction: ProxyDirection::TuiToUpstream,
                value: json!({
                    "method":"turn/start",
                    "id":7,
                    "params":{"threadId":"thread-crash","input":[]}
                }),
            };
            let mut boundary = None;
            if !matches!(crash_after, CrashAfter::BeforeRaw) {
                sink.commit_envelope(&envelope)?;
            }
            if matches!(
                crash_after,
                CrashAfter::Dispatch | CrashAfter::WriteComplete | CrashAfter::Response
            ) {
                boundary = sink.prepare_upstream_write(
                    &envelope,
                    "turn/start",
                    ProtocolClassification::KnownMutation,
                    Some(&ProxyOwner {
                        owner_type: "terminal".into(),
                        owner_id: "attachment-crash".into(),
                        principal_id: "local_bearer".into(),
                        input_lease_id: Some("input-crash".into()),
                    }),
                )?;
            }
            if matches!(
                crash_after,
                CrashAfter::WriteComplete | CrashAfter::Response
            ) {
                sink.upstream_write_completed(
                    boundary
                        .as_ref()
                        .context("write-complete boundary missing")?,
                )?;
            }
            if matches!(crash_after, CrashAfter::Response) {
                sink.upstream_response_committed(
                    boundary.as_ref().context("response boundary missing")?,
                    &json!({"id":7,"result":{"turn":{"id":"turn-crash"}}}),
                )?;
            }
            let command_id = boundary.map(|value| value.command_id);
            drop(sink);
            drop(writer);
            drop(database);

            let reopened = Database::open(&path)?;
            reopened.migrate()?;
            let report = reopened.recover_gateway_after_restart()?;
            let raw_count: i64 = reopened.connect()?.query_row(
                "SELECT COUNT(*) FROM raw_events WHERE worker_id='worker-crash'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(raw_count, expected_raw, "raw boundary mismatch for {case}");
            match (command_id, expected_state) {
                (None, None) => assert_eq!(report.outcome_unknown, 0, "{case}"),
                (Some(command_id), Some(expected_state)) => {
                    let command = reopened
                        .gateway_command(&command_id)?
                        .with_context(|| format!("missing recovered command for {case}"))?;
                    assert_eq!(command.state, expected_state, "command state for {case}");
                    assert_eq!(
                        report.outcome_unknown,
                        usize::from(expected_state == "outcome_unknown"),
                        "recovery classification for {case}"
                    );
                }
                _ => anyhow::bail!("invalid crash matrix expectation for {case}"),
            }
            let second_recovery = reopened.recover_gateway_after_restart()?;
            assert_eq!(second_recovery.closed_epochs, 0, "{case}");
            assert_eq!(second_recovery.failed_before_dispatch, 0, "{case}");
            assert_eq!(second_recovery.outcome_unknown, 0, "{case}");
            assert!(second_recovery.image_paths.is_empty(), "{case}");
        }
        Ok(())
    }

    #[test]
    fn clear_fork_and_side_threads_update_one_worker_lease_set_at_protocol_boundaries() -> Result<()>
    {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let writer = WriterHandle::start(database.clone(), 128, 128, 16)?;
        writer.upsert_source_kind(
            "source-1",
            "app_server",
            "fixture-upstream",
            &json!({}),
            "ready",
        )?;
        writer.record_live_capabilities("source-1", "source-epoch-1", &json!({}))?;
        let create_command_id = "create-worker-1";
        assert!(matches!(
            writer.receive_gateway_command(NewGatewayCommand {
                command_id: create_command_id.into(),
                principal_id: "local_bearer".into(),
                capability: "session.create".into(),
                idempotency_key: "create-worker-key-1".into(),
                payload_hash: "create-worker-hash-1".into(),
                target: GatewayCommandTarget {
                    source_id: "source-1".into(),
                    source_epoch: "source-epoch-1".into(),
                    thread_key: None,
                    codex_thread_id: Some("thread-old".into()),
                    expected_turn_id: None,
                    expected_request_id: None,
                    expected_request_version: None,
                },
                input_summary_json: json!({"mode":"resume"}).to_string(),
                origin: GatewayCommandOrigin::WorkerControl,
            })?,
            ReceiveGatewayCommand::Created(_)
        ));
        assert!(matches!(
            writer.register_session_worker(SessionWorkerRegistration {
                worker_id: "worker-1".into(),
                create_command_id: create_command_id.into(),
                principal_id: "local_bearer".into(),
                source_id: "source-1".into(),
                source_epoch: "source-epoch-1".into(),
                mode: "resume".into(),
                canonical_cwd: "/synthetic".into(),
                rows: 24,
                cols: 80,
                runtime_dir_name: "worker-1".into(),
                primary_lease_id: "lease-old".into(),
                codex_thread_id: Some("thread-old".into()),
                reservation_id: None,
            })?,
            RegisterSessionWorker::Created { .. }
        ));
        for state in ["authorized", "dispatching", "accepted_by_source"] {
            writer.transition_gateway_command(GatewayTransition {
                command_id: create_command_id.into(),
                to_state: state.into(),
                result_summary_json: None,
                error_code: None,
                error_message: None,
                reason_code: None,
                decision: "allow".into(),
                outcome: state.into(),
            })?;
        }
        let sink = SessionProxyEventSink::new(
            writer,
            database.clone(),
            [3; 32],
            Arc::new(SessionProtocolBridge::default()),
            "worker-1".into(),
            "lease-old".into(),
            create_command_id.into(),
            Some("thread-old".into()),
        );
        let owner = ProxyOwner {
            owner_type: "terminal".into(),
            owner_id: "attachment-1".into(),
            principal_id: "local_bearer".into(),
            input_lease_id: Some("input-1".into()),
        };
        let envelope = |seq, method: &str, params: Value| ProxyEnvelope {
            source_id: "source-1".into(),
            store_source_id: "store-1".into(),
            source_epoch: "source-epoch-1".into(),
            worker_id: "worker-1".into(),
            connection_epoch: "connection-1".into(),
            proxy_seq: seq,
            direction: ProxyDirection::TuiToUpstream,
            value: json!({"id":seq,"method":method,"params":params}),
        };

        let unsubscribe = envelope(1, "thread/unsubscribe", json!({"threadId":"thread-old"}));
        let boundary = sink
            .prepare_upstream_write(
                &unsubscribe,
                "thread/unsubscribe",
                ProtocolClassification::KnownMutation,
                Some(&owner),
            )?
            .context("unsubscribe boundary missing")?;
        sink.upstream_write_completed(&boundary)?;
        sink.upstream_response_committed(&boundary, &json!({"id":1,"result":{}}))?;

        let duplicate_unsubscribe =
            envelope(20, "thread/unsubscribe", json!({"threadId":"thread-old"}));
        let duplicate_boundary = sink
            .prepare_upstream_write(
                &duplicate_unsubscribe,
                "thread/unsubscribe",
                ProtocolClassification::KnownMutation,
                Some(&owner),
            )?
            .context("duplicate unsubscribe boundary missing")?;
        sink.upstream_write_completed(&duplicate_boundary)?;
        sink.upstream_response_committed(&duplicate_boundary, &json!({"id":20,"result":{}}))?;

        let start = envelope(2, "thread/start", json!({}));
        let boundary = sink
            .prepare_upstream_write(
                &start,
                "thread/start",
                ProtocolClassification::KnownMutation,
                Some(&owner),
            )?
            .context("start boundary missing")?;
        sink.upstream_write_completed(&boundary)?;
        sink.upstream_response_committed(
            &boundary,
            &json!({"id":2,"result":{"thread":{"id":"thread-new"}}}),
        )?;

        let fork = envelope(3, "thread/fork", json!({"threadId":"thread-new"}));
        let boundary = sink
            .prepare_upstream_write(
                &fork,
                "thread/fork",
                ProtocolClassification::KnownMutation,
                Some(&owner),
            )?
            .context("fork boundary missing")?;
        sink.upstream_write_completed(&boundary)?;
        sink.upstream_response_committed(
            &boundary,
            &json!({"id":3,"result":{"thread":{"id":"thread-child"}}}),
        )?;

        let uncertain_fork = envelope(4, "thread/fork", json!({"threadId":"thread-new"}));
        let uncertain_boundary = sink
            .prepare_upstream_write(
                &uncertain_fork,
                "thread/fork",
                ProtocolClassification::KnownMutation,
                Some(&owner),
            )?
            .context("uncertain fork boundary missing")?;
        sink.upstream_write_completed(&uncertain_boundary)?;
        sink.outcome_unknown(&uncertain_boundary, "UPSTREAM_DISCONNECTED")?;

        sink.observe_upstream_thread(&ProxyEnvelope {
            direction: ProxyDirection::UpstreamToTui,
            proxy_seq: 5,
            value: json!({"method":"thread/status/changed","params":{"threadId":"thread-side"}}),
            ..envelope(5, "unused", json!({}))
        })?;

        let worker = database
            .session_worker("worker-1")?
            .context("missing worker")?;
        assert_eq!(worker.primary_thread_id.as_deref(), Some("thread-new"));
        let leases = database.thread_leases_for_worker("worker-1")?;
        assert_eq!(
            leases
                .iter()
                .filter(|lease| lease.codex_thread_id.as_deref() == Some("thread-old"))
                .count(),
            1,
            "duplicate unsubscribe must not manufacture a side lease"
        );
        assert!(leases.iter().any(|lease| {
            lease.codex_thread_id.as_deref() == Some("thread-old")
                && lease.state == "released"
                && lease.role == "primary"
        }));
        assert!(leases.iter().any(|lease| {
            lease.codex_thread_id.as_deref() == Some("thread-new")
                && lease.state == "active"
                && lease.role == "primary"
        }));
        assert!(leases.iter().any(|lease| {
            lease.codex_thread_id.as_deref() == Some("thread-child")
                && lease.state == "active"
                && lease.role == "child"
        }));
        assert!(leases.iter().any(|lease| {
            lease.codex_thread_id.as_deref() == Some("thread-side")
                && lease.state == "active"
                && lease.role == "side"
        }));
        assert!(leases.iter().any(|lease| {
            lease.codex_thread_id.is_none() && lease.state == "orphaned" && lease.role == "child"
        }));

        let turn_start = envelope(
            10,
            "turn/start",
            json!({"threadId":"thread-new","input":[{"type":"text","text":"private"}]}),
        );
        sink.commit_envelope(&turn_start)?;
        let turn_boundary = sink
            .prepare_upstream_write(
                &turn_start,
                "turn/start",
                ProtocolClassification::KnownMutation,
                Some(&owner),
            )?
            .context("turn/start boundary missing")?;
        sink.upstream_write_completed(&turn_boundary)?;
        sink.commit_envelope(&ProxyEnvelope {
            direction: ProxyDirection::UpstreamToTui,
            proxy_seq: 11,
            value: json!({
                "method":"turn/started",
                "params":{"threadId":"thread-new","turn":{"id":"turn-1","status":"inProgress"}}
            }),
            ..turn_start.clone()
        })?;
        sink.upstream_response_committed(
            &turn_boundary,
            &json!({"id":10,"result":{"turn":{"id":"turn-1","status":"inProgress"}}}),
        )?;
        let active = database
            .active_turn_owner("source-1", "source-epoch-1", "thread-new", "turn-1")?
            .context("active TurnOwner missing")?;
        assert_eq!(active.principal_id, "local_bearer");
        assert_eq!(active.input_lease_id.as_deref(), Some("input-1"));

        let changed_input_owner = ProxyOwner {
            owner_type: "terminal".into(),
            owner_id: "attachment-2".into(),
            principal_id: "other-browser".into(),
            input_lease_id: Some("input-2".into()),
        };
        let steer = envelope(
            12,
            "turn/steer",
            json!({"threadId":"thread-new","expectedTurnId":"turn-1","input":[]}),
        );
        let steer_boundary = sink
            .prepare_upstream_write(
                &steer,
                "turn/steer",
                ProtocolClassification::KnownMutation,
                Some(&changed_input_owner),
            )?
            .context("turn/steer boundary missing")?;
        assert_eq!(
            database
                .gateway_command(&steer_boundary.command_id)?
                .context("steer command missing")?
                .principal_id,
            "local_bearer",
            "active Turn ownership must stay frozen when InputLease changes"
        );
        sink.upstream_write_completed(&steer_boundary)?;
        sink.upstream_response_committed(
            &steer_boundary,
            &json!({"id":12,"result":{"turnId":"turn-1"}}),
        )?;
        sink.commit_envelope(&ProxyEnvelope {
            direction: ProxyDirection::UpstreamToTui,
            proxy_seq: 13,
            value: json!({
                "method":"turn/completed",
                "params":{"threadId":"thread-new","turn":{"id":"turn-1","status":"interrupted"}}
            }),
            ..turn_start
        })?;
        assert!(
            database
                .active_turn_owner("source-1", "source-epoch-1", "thread-new", "turn-1")?
                .is_none()
        );
        database.validate_session_transition_consistency_for_test()?;
        Ok(())
    }

    #[test]
    fn channel_turn_owner_routes_request_away_from_tui_and_completes_cas_state() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let writer = WriterHandle::start(database.clone(), 128, 128, 16)?;
        writer.upsert_source_kind(
            "source-1",
            "app_server",
            "fixture-upstream",
            &json!({}),
            "ready",
        )?;
        writer.record_live_capabilities("source-1", "source-epoch-1", &json!({}))?;
        let create_command_id = "create-channel-worker";
        writer.receive_gateway_command(NewGatewayCommand {
            command_id: create_command_id.into(),
            principal_id: "local_bearer".into(),
            capability: "session.create".into(),
            idempotency_key: "create-channel-worker-key".into(),
            payload_hash: "create-channel-worker-hash".into(),
            target: GatewayCommandTarget {
                source_id: "source-1".into(),
                source_epoch: "source-epoch-1".into(),
                thread_key: Some(thread_key("store-1", "thread-1")),
                codex_thread_id: Some("thread-1".into()),
                expected_turn_id: None,
                expected_request_id: None,
                expected_request_version: None,
            },
            input_summary_json: "{}".into(),
            origin: GatewayCommandOrigin::WorkerControl,
        })?;
        assert!(matches!(
            writer.register_session_worker(SessionWorkerRegistration {
                worker_id: "worker-1".into(),
                create_command_id: create_command_id.into(),
                principal_id: "local_bearer".into(),
                source_id: "source-1".into(),
                source_epoch: "source-epoch-1".into(),
                mode: "resume".into(),
                canonical_cwd: "/synthetic".into(),
                rows: 24,
                cols: 80,
                runtime_dir_name: "worker-1".into(),
                primary_lease_id: "lease-1".into(),
                codex_thread_id: Some("thread-1".into()),
                reservation_id: None,
            })?,
            RegisterSessionWorker::Created { .. }
        ));
        for state in ["authorized", "dispatching", "accepted_by_source"] {
            transition_command(&writer, create_command_id, state, None, None)?;
        }
        writer.upsert_turn_owner(TurnOwnerRecord {
            source_id: "source-1".into(),
            source_epoch: "source-epoch-1".into(),
            worker_id: "worker-1".into(),
            codex_thread_id: "thread-1".into(),
            codex_turn_id: "turn-1".into(),
            owner_type: "channel".into(),
            owner_id: "fake:conversation".into(),
            principal_id: "channel:fixture".into(),
            input_lease_id: None,
            state: "active".into(),
            version: 1,
            start_command_id: None,
        })?;
        let sink = SessionProxyEventSink::new(
            writer.clone(),
            database.clone(),
            [7; 32],
            Arc::new(SessionProtocolBridge::default()),
            "worker-1".into(),
            "lease-1".into(),
            create_command_id.into(),
            Some("thread-1".into()),
        );
        let request_id = serde_json::to_string(&json!("question-1"))?;
        let request = ProxyEnvelope {
            source_id: "source-1".into(),
            store_source_id: "store-1".into(),
            source_epoch: "source-epoch-1".into(),
            worker_id: "worker-1".into(),
            connection_epoch: "connection-1".into(),
            proxy_seq: 1,
            direction: ProxyDirection::UpstreamToTui,
            value: json!({
                "method":"item/tool/requestUserInput",
                "id":"question-1",
                "params":{"threadId":"thread-1","turnId":"turn-1","questions":[]}
            }),
        };
        sink.commit_envelope(&request)?;
        assert!(matches!(
            sink.server_request_route(&request)?,
            ServerRequestRoute::Channel { .. }
        ));
        assert_eq!(
            database
                .pending_request_target("source-1", "source-epoch-1", &request_id)?
                .context("pending request missing")?
                .state,
            "pending"
        );
        let action_command_id = "request-action-1";
        writer.receive_gateway_command(NewGatewayCommand {
            command_id: action_command_id.into(),
            principal_id: "channel:fixture".into(),
            capability: "request.action".into(),
            idempotency_key: "request-action-key-1".into(),
            payload_hash: "request-action-hash-1".into(),
            target: GatewayCommandTarget {
                source_id: "source-1".into(),
                source_epoch: "source-epoch-1".into(),
                thread_key: Some(thread_key("store-1", "thread-1")),
                codex_thread_id: Some("thread-1".into()),
                expected_turn_id: Some("turn-1".into()),
                expected_request_id: Some(request_id.clone()),
                expected_request_version: Some(1),
            },
            input_summary_json: "{}".into(),
            origin: GatewayCommandOrigin::Channel,
        })?;
        transition_command(&writer, action_command_id, "authorized", None, None)?;
        assert!(matches!(
            writer.claim_pending_request(action_command_id)?,
            crate::store::PendingRequestClaim::Claimed(_)
        ));
        let boundary = MutationBoundary {
            command_id: action_command_id.into(),
        };
        sink.upstream_write_completed(&boundary)?;
        sink.upstream_response_committed(&boundary, &json!({"result":{"requestId":request_id}}))?;
        let target = database
            .pending_request_target("source-1", "source-epoch-1", &request_id)?
            .context("resolved request missing")?;
        assert_eq!(
            (target.state.as_str(), target.request_version),
            ("resolved", 2)
        );
        assert_eq!(
            database
                .gateway_command(action_command_id)?
                .context("request command missing")?
                .state,
            "completed"
        );
        Ok(())
    }

    #[test]
    fn terminal_response_is_audited_and_resolves_the_pending_request_once() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let writer = WriterHandle::start(database.clone(), 128, 128, 16)?;
        writer.upsert_source_kind(
            "source-1",
            "app_server",
            "fixture-upstream",
            &json!({}),
            "ready",
        )?;
        writer.record_live_capabilities("source-1", "source-epoch-1", &json!({}))?;
        let create_command_id = "create-terminal-worker";
        writer.receive_gateway_command(NewGatewayCommand {
            command_id: create_command_id.into(),
            principal_id: "local_bearer".into(),
            capability: "session.create".into(),
            idempotency_key: "create-terminal-worker-key".into(),
            payload_hash: "create-terminal-worker-hash".into(),
            target: GatewayCommandTarget {
                source_id: "source-1".into(),
                source_epoch: "source-epoch-1".into(),
                thread_key: Some(thread_key("store-1", "thread-1")),
                codex_thread_id: Some("thread-1".into()),
                expected_turn_id: None,
                expected_request_id: None,
                expected_request_version: None,
            },
            input_summary_json: "{}".into(),
            origin: GatewayCommandOrigin::WorkerControl,
        })?;
        assert!(matches!(
            writer.register_session_worker(SessionWorkerRegistration {
                worker_id: "worker-1".into(),
                create_command_id: create_command_id.into(),
                principal_id: "local_bearer".into(),
                source_id: "source-1".into(),
                source_epoch: "source-epoch-1".into(),
                mode: "resume".into(),
                canonical_cwd: "/synthetic".into(),
                rows: 24,
                cols: 80,
                runtime_dir_name: "worker-1".into(),
                primary_lease_id: "lease-1".into(),
                codex_thread_id: Some("thread-1".into()),
                reservation_id: None,
            })?,
            RegisterSessionWorker::Created { .. }
        ));
        for state in ["authorized", "dispatching", "accepted_by_source"] {
            transition_command(&writer, create_command_id, state, None, None)?;
        }
        writer.upsert_turn_owner(TurnOwnerRecord {
            source_id: "source-1".into(),
            source_epoch: "source-epoch-1".into(),
            worker_id: "worker-1".into(),
            codex_thread_id: "thread-1".into(),
            codex_turn_id: "turn-1".into(),
            owner_type: "terminal".into(),
            owner_id: "attachment-1".into(),
            principal_id: "local_cookie".into(),
            input_lease_id: Some("input-1".into()),
            state: "active".into(),
            version: 1,
            start_command_id: None,
        })?;
        let sink = SessionProxyEventSink::new(
            writer,
            database.clone(),
            [8; 32],
            Arc::new(SessionProtocolBridge::default()),
            "worker-1".into(),
            "lease-1".into(),
            create_command_id.into(),
            Some("thread-1".into()),
        );
        let request_id = serde_json::to_string(&json!(37))?;
        let request_payload = json!({
            "threadId":"thread-1",
            "turnId":"turn-1",
            "questions":[{"id":"q1"}]
        });
        let request = ProxyEnvelope {
            source_id: "source-1".into(),
            store_source_id: "store-1".into(),
            source_epoch: "source-epoch-1".into(),
            worker_id: "worker-1".into(),
            connection_epoch: "connection-1".into(),
            proxy_seq: 1,
            direction: ProxyDirection::UpstreamToTui,
            value: json!({
                "method":"item/tool/requestUserInput",
                "id":37,
                "params":request_payload
            }),
        };
        sink.commit_envelope(&request)?;
        assert_eq!(
            sink.server_request_route(&request)?,
            ServerRequestRoute::Terminal
        );
        let response = ProxyEnvelope {
            proxy_seq: 2,
            direction: ProxyDirection::TuiToUpstream,
            value: json!({"id":37,"result":{"answers":{"q1":{"answers":["yes"]}}}}),
            ..request
        };
        sink.commit_envelope(&response)?;
        let boundary = sink
            .prepare_server_response(
                &response,
                &request_id,
                "item/tool/requestUserInput",
                &request_payload,
            )?
            .context("terminal response boundary missing")?;
        sink.upstream_write_completed(&boundary)?;
        sink.upstream_response_committed(&boundary, &json!({"result":{"requestId":request_id}}))?;

        let target = database
            .pending_request_target("source-1", "source-epoch-1", &request_id)?
            .context("resolved request missing")?;
        assert_eq!(
            (target.state.as_str(), target.request_version),
            ("resolved", 2)
        );
        let command = database
            .gateway_command(&boundary.command_id)?
            .context("terminal response command missing")?;
        assert_eq!(command.state, "completed");
        assert_eq!(command.principal_id, "local_cookie");
        assert_eq!(
            database.query_json(
                "SELECT origin FROM gateway_commands WHERE command_id=?1",
                &[&boundary.command_id],
                |row| Ok(json!({"origin":row.get::<_, String>(0)?})),
            )?[0]["origin"],
            "tui"
        );
        let duplicate = sink
            .prepare_server_response(
                &ProxyEnvelope {
                    proxy_seq: 3,
                    ..response
                },
                &request_id,
                "item/tool/requestUserInput",
                &request_payload,
            )
            .expect_err("terminal response must win CAS only once");
        assert!(duplicate.to_string().contains("REQUEST_NOT_PENDING"));
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedStep {
    Raw(u64, ProxyDirection),
    DecodeError(u64, ProxyDirection),
    Prepared(u64, ProtocolClassification),
    Written(String),
    Responded(String),
    OutcomeUnknown(String),
    Anomaly(u64, &'static str),
}

pub struct RecordingEventSink {
    steps: Mutex<Vec<RecordedStep>>,
    fail_raw_at: Mutex<Option<u64>>,
    server_request_route: Mutex<ServerRequestRoute>,
}

impl Default for RecordingEventSink {
    fn default() -> Self {
        Self {
            steps: Mutex::new(Vec::new()),
            fail_raw_at: Mutex::new(None),
            server_request_route: Mutex::new(ServerRequestRoute::Terminal),
        }
    }
}

impl RecordingEventSink {
    pub fn steps(&self) -> Vec<RecordedStep> {
        self.steps.lock().expect("recording sink poisoned").clone()
    }

    pub fn fail_raw_at(&self, seq: u64) {
        *self.fail_raw_at.lock().expect("recording sink poisoned") = Some(seq);
    }

    pub fn set_server_request_route(&self, route: ServerRequestRoute) {
        *self
            .server_request_route
            .lock()
            .expect("recording sink poisoned") = route;
    }
}

impl ProxyEventSink for RecordingEventSink {
    fn server_request_route(&self, _envelope: &ProxyEnvelope) -> Result<ServerRequestRoute> {
        Ok(self
            .server_request_route
            .lock()
            .expect("recording sink poisoned")
            .clone())
    }

    fn commit_envelope(&self, envelope: &ProxyEnvelope) -> Result<()> {
        if *self.fail_raw_at.lock().expect("recording sink poisoned") == Some(envelope.proxy_seq) {
            anyhow::bail!("injected raw commit failure");
        }
        self.steps
            .lock()
            .expect("recording sink poisoned")
            .push(RecordedStep::Raw(envelope.proxy_seq, envelope.direction));
        Ok(())
    }

    fn commit_decode_error(
        &self,
        envelope: &ProxyEnvelope,
        _raw_text: &str,
        _error_code: &'static str,
    ) -> Result<()> {
        if *self.fail_raw_at.lock().expect("recording sink poisoned") == Some(envelope.proxy_seq) {
            anyhow::bail!("injected raw commit failure");
        }
        self.steps
            .lock()
            .expect("recording sink poisoned")
            .push(RecordedStep::DecodeError(
                envelope.proxy_seq,
                envelope.direction,
            ));
        Ok(())
    }

    fn prepare_upstream_write(
        &self,
        envelope: &ProxyEnvelope,
        _method: &str,
        classification: ProtocolClassification,
        owner: Option<&ProxyOwner>,
    ) -> Result<Option<MutationBoundary>> {
        if classification == ProtocolClassification::KnownReadOnly {
            return Ok(None);
        }
        owner.context("INPUT_ATTRIBUTION_UNKNOWN")?;
        self.steps
            .lock()
            .expect("recording sink poisoned")
            .push(RecordedStep::Prepared(envelope.proxy_seq, classification));
        Ok(Some(MutationBoundary {
            command_id: format!("command-{}", envelope.proxy_seq),
        }))
    }

    fn upstream_write_completed(&self, boundary: &MutationBoundary) -> Result<()> {
        self.steps
            .lock()
            .expect("recording sink poisoned")
            .push(RecordedStep::Written(boundary.command_id.clone()));
        Ok(())
    }

    fn upstream_response_committed(
        &self,
        boundary: &MutationBoundary,
        _response: &Value,
    ) -> Result<()> {
        self.steps
            .lock()
            .expect("recording sink poisoned")
            .push(RecordedStep::Responded(boundary.command_id.clone()));
        Ok(())
    }

    fn outcome_unknown(&self, boundary: &MutationBoundary, _reason: &'static str) -> Result<()> {
        self.steps
            .lock()
            .expect("recording sink poisoned")
            .push(RecordedStep::OutcomeUnknown(boundary.command_id.clone()));
        Ok(())
    }

    fn protocol_anomaly(&self, envelope: &ProxyEnvelope, code: &'static str) -> Result<()> {
        self.steps
            .lock()
            .expect("recording sink poisoned")
            .push(RecordedStep::Anomaly(envelope.proxy_seq, code));
        Ok(())
    }
}
