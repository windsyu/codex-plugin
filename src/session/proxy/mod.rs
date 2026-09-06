mod event_sink;
mod id_map;

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, client_async};
use uuid::Uuid;

use crate::domain::gateway::{PendingRequestAction, request_action_response};
use crate::domain::live::validate_app_server_envelope;
use crate::permissions::validate_private_unix_socket;

#[allow(unused_imports)] // Recording exports are used only by proxy contract tests.
pub use event_sink::{
    MutationBoundary, ProxyEnvelope, ProxyEventSink, RecordedStep, RecordingEventSink,
    SessionProtocolBridge, SessionProxyEventSink, WriterProxyEventSink,
};
use id_map::RequestIdMap;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyDirection {
    TuiToUpstream,
    UpstreamToTui,
}

impl ProxyDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TuiToUpstream => "tui_to_upstream",
            Self::UpstreamToTui => "upstream_to_tui",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolClassification {
    KnownReadOnly,
    KnownMutation,
    UnknownPossibleMutation,
}

impl ProtocolClassification {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::KnownReadOnly => "known_read_only",
            Self::KnownMutation => "known_mutation",
            Self::UnknownPossibleMutation => "unknown_possible_mutation",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProxyOwner {
    pub owner_type: String,
    pub owner_id: String,
    pub principal_id: String,
    pub input_lease_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerRequestRoute {
    Terminal,
    Channel {
        owner_type: String,
        owner_id: String,
        principal_id: String,
    },
    Unattributed,
}

pub struct ProxyConfig {
    pub worker_id: String,
    pub source_id: String,
    pub store_source_id: String,
    pub source_epoch: String,
    pub upstream_socket: PathBuf,
    pub private_socket: PathBuf,
    pub event_sink: Arc<dyn ProxyEventSink>,
}

pub struct ProxyHandle {
    socket_path: PathBuf,
    owner: Arc<RwLock<Option<ProxyOwner>>>,
    expected_downstream_pid: watch::Sender<Option<u32>>,
    control: ProxyControlHandle,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<()>>>,
}

#[derive(Clone)]
pub struct ProxyOwnerHandle {
    owner: Arc<RwLock<Option<ProxyOwner>>>,
}

#[derive(Clone)]
pub struct ProxyControlHandle {
    sender: mpsc::Sender<ProxyControl>,
}

struct ProxyControl {
    action: ClosedProxyAction,
    boundary: Option<MutationBoundary>,
    reply: oneshot::Sender<std::result::Result<Value, String>>,
}

enum ClosedProxyAction {
    Interrupt {
        thread_id: String,
        expected_turn_id: String,
    },
    PrepareRequest {
        request_id: String,
        action: PendingRequestAction,
    },
    ResolveRequest {
        request_id: String,
        response: Value,
    },
    HandoffRequestToTerminal {
        request_id: String,
    },
}

struct PendingProxyControl {
    boundary: MutationBoundary,
    reply: oneshot::Sender<std::result::Result<Value, String>>,
}

struct PendingServerRequest {
    rpc_id: Value,
    method: String,
    payload: Value,
    value: Value,
    route: ServerRequestRoute,
}

impl ProxyControlHandle {
    pub async fn interrupt(
        &self,
        thread_id: String,
        expected_turn_id: String,
        boundary: MutationBoundary,
    ) -> Result<Value> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(ProxyControl {
                action: ClosedProxyAction::Interrupt {
                    thread_id,
                    expected_turn_id,
                },
                boundary: Some(boundary),
                reply,
            })
            .await
            .context("Session proxy control channel closed")?;
        response
            .await
            .context("Session proxy interrupt response channel closed")?
            .map_err(anyhow::Error::msg)
    }

    pub async fn prepare_request_action(
        &self,
        request_id: String,
        action: PendingRequestAction,
    ) -> Result<Value> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(ProxyControl {
                action: ClosedProxyAction::PrepareRequest { request_id, action },
                boundary: None,
                reply,
            })
            .await
            .context("Session proxy control channel closed")?;
        response
            .await
            .context("Session proxy request validation channel closed")?
            .map_err(anyhow::Error::msg)
    }

    pub async fn resolve_request(
        &self,
        request_id: String,
        response_value: Value,
        boundary: MutationBoundary,
    ) -> Result<Value> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(ProxyControl {
                action: ClosedProxyAction::ResolveRequest {
                    request_id,
                    response: response_value,
                },
                boundary: Some(boundary),
                reply,
            })
            .await
            .context("Session proxy control channel closed")?;
        response
            .await
            .context("Session proxy request response channel closed")?
            .map_err(anyhow::Error::msg)
    }

    pub async fn handoff_request_to_terminal(&self, request_id: String) -> Result<Value> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(ProxyControl {
                action: ClosedProxyAction::HandoffRequestToTerminal { request_id },
                boundary: None,
                reply,
            })
            .await
            .context("Session proxy request handoff channel closed")?;
        response
            .await
            .context("Session proxy request handoff response channel closed")?
            .map_err(anyhow::Error::msg)
    }
}

impl ProxyOwnerHandle {
    pub fn set(&self, owner: Option<ProxyOwner>) {
        *self.owner.write().expect("proxy owner poisoned") = owner;
    }
}

impl ProxyHandle {
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn control_handle(&self) -> ProxyControlHandle {
        self.control.clone()
    }

    pub fn authorize_downstream_pid(&self, pid: u32) -> Result<()> {
        if pid == 0 {
            anyhow::bail!("private proxy downstream PID must be non-zero");
        }
        let authorized = self.expected_downstream_pid.send_if_modified(|current| {
            if current.is_some() {
                false
            } else {
                *current = Some(pid);
                true
            }
        });
        if !authorized {
            anyhow::bail!("private proxy downstream PID is already authorized");
        }
        if self.expected_downstream_pid.receiver_count() == 0 {
            anyhow::bail!("private proxy stopped before downstream authorization");
        }
        Ok(())
    }

    pub fn set_input_owner(&self, owner: Option<ProxyOwner>) {
        *self.owner.write().expect("proxy owner poisoned") = owner;
    }

    pub fn owner_handle(&self) -> ProxyOwnerHandle {
        ProxyOwnerHandle {
            owner: self.owner.clone(),
        }
    }

    pub async fn shutdown(mut self) -> Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let task = self.task.take().context("proxy task is unavailable")?;
        task.await.context("proxy task panicked")?
    }
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

pub struct ProxyServer;

impl ProxyServer {
    pub fn bind(config: ProxyConfig) -> Result<ProxyHandle> {
        validate_private_socket_parent(&config.private_socket)?;
        let listener = UnixListener::bind(&config.private_socket).with_context(|| {
            format!(
                "bind private App Server proxy {}",
                config.private_socket.display()
            )
        })?;
        #[cfg(unix)]
        std::fs::set_permissions(
            &config.private_socket,
            std::fs::Permissions::from_mode(0o600),
        )
        .with_context(|| {
            format!(
                "set private App Server proxy permissions {}",
                config.private_socket.display()
            )
        })?;
        let socket_path = config.private_socket.clone();
        let owner = Arc::new(RwLock::new(None));
        let task_owner = owner.clone();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (control_tx, control_rx) = mpsc::channel(32);
        let (expected_downstream_pid, expected_downstream_pid_rx) = watch::channel(None);
        let task = tokio::spawn(run_proxy(
            listener,
            config,
            task_owner,
            expected_downstream_pid_rx,
            control_rx,
            shutdown_rx,
        ));
        Ok(ProxyHandle {
            socket_path,
            owner,
            expected_downstream_pid,
            control: ProxyControlHandle { sender: control_tx },
            shutdown: Some(shutdown_tx),
            task: Some(task),
        })
    }
}

async fn run_proxy(
    listener: UnixListener,
    config: ProxyConfig,
    owner: Arc<RwLock<Option<ProxyOwner>>>,
    mut expected_downstream_pid: watch::Receiver<Option<u32>>,
    mut control: mpsc::Receiver<ProxyControl>,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    let _socket_cleanup = SocketCleanup(config.private_socket.clone());
    let expected_downstream_pid = loop {
        if let Some(pid) = *expected_downstream_pid.borrow() {
            break pid;
        }
        tokio::select! {
            changed = expected_downstream_pid.changed() => changed.context("private proxy downstream authorization channel closed")?,
            _ = &mut shutdown => return Ok(()),
        }
    };
    let downstream = tokio::select! {
        accepted = listener.accept() => accepted?.0,
        _ = &mut shutdown => return Ok(()),
    };
    validate_peer(&downstream, expected_downstream_pid)?;
    let mut downstream = accept_async(downstream).await?;
    validate_private_unix_socket(&config.upstream_socket, "configured app-server endpoint")?;
    let upstream_stream = UnixStream::connect(&config.upstream_socket)
        .await
        .with_context(|| {
            format!(
                "connect upstream App Server {}",
                config.upstream_socket.display()
            )
        })?;
    let (mut upstream, _) = client_async("ws://localhost/", upstream_stream).await?;
    let connection_epoch = Uuid::new_v4().to_string();
    config.event_sink.connection_opened(
        &config.worker_id,
        &config.source_id,
        &config.source_epoch,
        &connection_epoch,
    )?;
    let mut proxy_seq = 0_u64;
    let mut ids = RequestIdMap::default();
    let mut next_control_id = 0_u64;
    let mut pending_controls = std::collections::HashMap::<String, PendingProxyControl>::new();
    let mut pending_notifications = Vec::<MutationBoundary>::new();
    let mut pending_server_requests =
        std::collections::HashMap::<String, PendingServerRequest>::new();
    let result: Result<()> = async {
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                second = listener.accept() => {
                    if let Ok((stream, _)) = second {
                        tokio::spawn(reject_second_downstream(stream));
                    }
                }
                command = control.recv() => {
                    let Some(command) = command else { continue };
                    match command.action {
                        ClosedProxyAction::PrepareRequest { request_id, action } => {
                            let prepared = pending_server_requests
                                .get(&request_id)
                                .ok_or_else(|| "REQUEST_NOT_PENDING".to_string())
                                .and_then(|pending| {
                                    match pending.route {
                                        ServerRequestRoute::Terminal => {
                                            return Err("REQUEST_OWNED_BY_TERMINAL".to_string());
                                        }
                                        ServerRequestRoute::Unattributed => {
                                            return Err("REQUEST_OWNER_UNATTRIBUTED".to_string());
                                        }
                                        ServerRequestRoute::Channel { .. } => {}
                                    }
                                    request_action_response(&pending.method, &pending.payload, &action)
                                        .map(|(response, _)| response)
                                        .map_err(|(code, _)| code.to_string())
                                });
                            let _ = command.reply.send(prepared);
                        }
                        ClosedProxyAction::ResolveRequest { request_id, response } => {
                            let Some(pending) = pending_server_requests.remove(&request_id) else {
                                let _ = command.reply.send(Err("REQUEST_NOT_PENDING".into()));
                                continue;
                            };
                            if !matches!(pending.route, ServerRequestRoute::Channel { .. }) {
                                pending_server_requests.insert(request_id, pending);
                                let _ = command.reply.send(Err("REQUEST_OWNED_BY_TERMINAL".into()));
                                continue;
                            }
                            let boundary = command.boundary.context("request resolution boundary missing")?;
                            proxy_seq = proxy_seq.checked_add(1).context("proxy sequence overflow")?;
                            let value = json!({"id":pending.rpc_id,"result":response});
                            let envelope = proxy_envelope(
                                &config,
                                &connection_epoch,
                                proxy_seq,
                                ProxyDirection::TuiToUpstream,
                                value.clone(),
                            );
                            config.event_sink.commit_envelope(&envelope)?;
                            if let Err(error) = upstream
                                .send(Message::Text(serde_json::to_string(&value)?.into()))
                                .await
                            {
                                let _ = config.event_sink.outcome_unknown(&boundary, "UPSTREAM_DISCONNECTED");
                                let _ = command.reply.send(Err("UPSTREAM_DISCONNECTED".into()));
                                return Err(error.into());
                            }
                            if let Err(error) = config.event_sink.upstream_write_completed(&boundary) {
                                let _ = config.event_sink.outcome_unknown(
                                    &boundary,
                                    "WRITE_COMMIT_FAILED",
                                );
                                let _ = command.reply.send(Err("OUTCOME_UNKNOWN".into()));
                                return Err(error);
                            }
                            if let Err(error) = config.event_sink.upstream_response_committed(
                                &boundary,
                                &json!({"result":{"requestId":request_id}}),
                            ) {
                                let _ = config.event_sink.outcome_unknown(
                                    &boundary,
                                    "RESPONSE_COMMIT_FAILED",
                                );
                                let _ = command.reply.send(Err("OUTCOME_UNKNOWN".into()));
                                return Err(error);
                            }
                            let _ = command.reply.send(Ok(json!({"accepted":true})));
                        }
                        ClosedProxyAction::HandoffRequestToTerminal { request_id } => {
                            let Some(pending) = pending_server_requests.get_mut(&request_id) else {
                                let _ = command.reply.send(Err("REQUEST_NOT_PENDING".into()));
                                continue;
                            };
                            if matches!(pending.route, ServerRequestRoute::Terminal) {
                                let _ = command.reply.send(Err("REQUEST_OWNED_BY_TERMINAL".into()));
                                continue;
                            }
                            if let Err(error) = downstream
                                .send(Message::Text(serde_json::to_string(&pending.value)?.into()))
                                .await
                            {
                                let _ = command.reply.send(Err("TERMINAL_DELIVERY_FAILED".into()));
                                return Err(error.into());
                            }
                            pending.route = ServerRequestRoute::Terminal;
                            let _ = command.reply.send(Ok(json!({
                                "requestId":request_id,
                                "deliveredTo":"terminal"
                            })));
                        }
                        ClosedProxyAction::Interrupt { thread_id, expected_turn_id } => {
                            let boundary = command.boundary.context("interrupt boundary missing")?;
                            proxy_seq = proxy_seq.checked_add(1).context("proxy sequence overflow")?;
                            next_control_id = next_control_id.checked_add(1).context("proxy control ID overflow")?;
                            let request_id = format!("gateway:control:{next_control_id}");
                            let value = json!({
                                "method":"turn/interrupt",
                                "id":request_id,
                                "params":{"threadId":thread_id,"turnId":expected_turn_id}
                            });
                            let envelope = proxy_envelope(
                                &config,
                                &connection_epoch,
                                proxy_seq,
                                ProxyDirection::TuiToUpstream,
                                value.clone(),
                            );
                            config.event_sink.commit_envelope(&envelope)?;
                            pending_controls.insert(request_id, PendingProxyControl {
                                boundary: boundary.clone(),
                                reply: command.reply,
                            });
                            upstream.send(Message::Text(serde_json::to_string(&value)?.into())).await?;
                            config.event_sink.upstream_write_completed(&boundary)?;
                        }
                    }
                }
                message = downstream.next() => {
                    let Some(message) = message else { break };
                    match message? {
                        Message::Text(text) => {
                            proxy_seq = proxy_seq.checked_add(1).context("proxy sequence overflow")?;
                            handle_tui_text(
                                text.as_str(), &config, ProxyPosition { connection_epoch: &connection_epoch, proxy_seq },
                                TuiRouting {
                                    owner: &owner,
                                    ids: &mut ids,
                                    pending_server_requests: &mut pending_server_requests,
                                    pending_notifications: &mut pending_notifications,
                                },
                                &mut downstream, &mut upstream,
                            ).await?;
                        }
                        Message::Close(frame) => {
                            let _ = upstream.send(Message::Close(frame)).await;
                            break;
                        }
                        Message::Ping(payload) => downstream.send(Message::Pong(payload)).await?,
                        Message::Pong(_) => {}
                        Message::Binary(_) | Message::Frame(_) => {
                            anyhow::bail!("proxy downstream sent an unsupported non-text frame");
                        }
                    }
                }
                message = upstream.next() => {
                    let Some(message) = message else { break };
                    match message? {
                        Message::Text(text) => {
                            proxy_seq = proxy_seq.checked_add(1).context("proxy sequence overflow")?;
                            handle_upstream_text(
                                text.as_str(), &config, ProxyPosition { connection_epoch: &connection_epoch, proxy_seq },
                                &mut ids, &mut downstream,
                                &mut pending_controls,
                                &mut pending_server_requests,
                            ).await?;
                        }
                        Message::Close(frame) => {
                            let _ = downstream.send(Message::Close(frame)).await;
                            break;
                        }
                        Message::Ping(payload) => upstream.send(Message::Pong(payload)).await?,
                        Message::Pong(_) => {}
                        Message::Binary(_) | Message::Frame(_) => {
                            anyhow::bail!("proxy upstream sent an unsupported non-text frame");
                        }
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    let pending_command_ids = ids.drain_command_ids();
    let had_pending_mutation = !pending_command_ids.is_empty() || !pending_notifications.is_empty();
    for command_id in pending_command_ids {
        let _ = config
            .event_sink
            .outcome_unknown(&MutationBoundary { command_id }, "UPSTREAM_DISCONNECTED");
    }
    for boundary in pending_notifications.drain(..) {
        let _ = config
            .event_sink
            .outcome_unknown(&boundary, "UPSTREAM_DISCONNECTED");
    }
    for (_, pending) in pending_controls.drain() {
        let _ = config
            .event_sink
            .outcome_unknown(&pending.boundary, "UPSTREAM_DISCONNECTED");
        let _ = pending.reply.send(Err("UPSTREAM_DISCONNECTED".into()));
    }
    let reason = if had_pending_mutation {
        "outcome_unknown"
    } else if result.is_ok() {
        "connection_closed"
    } else {
        "connection_failed"
    };
    let _ = config.event_sink.connection_closed(
        &config.worker_id,
        &connection_epoch,
        proxy_seq,
        reason,
    );
    result
}

struct SocketCleanup(PathBuf);

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.0.display(), error = %error, "failed to clean private proxy socket");
        }
    }
}

async fn reject_second_downstream(stream: UnixStream) {
    let Ok(mut websocket) = accept_async(stream).await else {
        return;
    };
    let _ = websocket
        .send(Message::Text(
            json!({
                "method":"gateway/proxy/error",
                "params":{"code":"PROXY_ALREADY_ATTACHED"}
            })
            .to_string()
            .into(),
        ))
        .await;
    let _ = websocket.close(None).await;
}

#[derive(Clone, Copy)]
struct ProxyPosition<'a> {
    connection_epoch: &'a str,
    proxy_seq: u64,
}

struct TuiRouting<'a> {
    owner: &'a Arc<RwLock<Option<ProxyOwner>>>,
    ids: &'a mut RequestIdMap,
    pending_server_requests: &'a mut std::collections::HashMap<String, PendingServerRequest>,
    pending_notifications: &'a mut Vec<MutationBoundary>,
}

async fn handle_tui_text(
    text: &str,
    config: &ProxyConfig,
    position: ProxyPosition<'_>,
    routing: TuiRouting<'_>,
    downstream: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
    upstream: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
) -> Result<()> {
    let ProxyPosition {
        connection_epoch,
        proxy_seq,
    } = position;
    let TuiRouting {
        owner,
        ids,
        pending_server_requests,
        pending_notifications,
    } = routing;
    let mut value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            let envelope = proxy_envelope(
                config,
                connection_epoch,
                proxy_seq,
                ProxyDirection::TuiToUpstream,
                Value::Null,
            );
            config
                .event_sink
                .commit_decode_error(&envelope, text, "INVALID_JSON")?;
            config
                .event_sink
                .protocol_anomaly(&envelope, "INVALID_JSON")?;
            send_protocol_error(downstream, Value::Null, "INVALID_JSON").await?;
            tracing::warn!(
                worker_id = %config.worker_id,
                connection_epoch,
                proxy_seq,
                error = %error,
                "ignored recoverable invalid TUI protocol frame"
            );
            return Ok(());
        }
    };
    let envelope = proxy_envelope(
        config,
        connection_epoch,
        proxy_seq,
        ProxyDirection::TuiToUpstream,
        value.clone(),
    );
    config.event_sink.commit_envelope(&envelope)?;
    if let Err(error) = validate_app_server_envelope(&value) {
        config
            .event_sink
            .protocol_anomaly(&envelope, "INVALID_ENVELOPE")?;
        send_protocol_error(
            downstream,
            value.get("id").cloned().unwrap_or(Value::Null),
            "INVALID_ENVELOPE",
        )
        .await?;
        tracing::warn!(
            worker_id = %config.worker_id,
            connection_epoch,
            proxy_seq,
            error = %error,
            "ignored recoverable invalid TUI protocol envelope"
        );
        return Ok(());
    }

    let Some(method) = value
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        let rpc_id = value.get("id").cloned().unwrap_or(Value::Null);
        let request_id = serde_json::to_string(&rpc_id)?;
        let Some(pending) = pending_server_requests.get(&request_id) else {
            config
                .event_sink
                .protocol_anomaly(&envelope, "UNKNOWN_SERVER_REQUEST_RESPONSE")?;
            send_protocol_error(downstream, rpc_id, "REQUEST_NOT_PENDING").await?;
            return Ok(());
        };
        if !matches!(pending.route, ServerRequestRoute::Terminal) {
            config
                .event_sink
                .protocol_anomaly(&envelope, "REQUEST_OWNED_BY_CHANNEL")?;
            send_protocol_error(downstream, rpc_id, "REQUEST_OWNED_BY_CHANNEL").await?;
            return Ok(());
        }
        let boundary = match config.event_sink.prepare_server_response(
            &envelope,
            &request_id,
            &pending.method,
            &pending.payload,
        ) {
            Ok(boundary) => boundary,
            Err(error) => {
                config
                    .event_sink
                    .protocol_anomaly(&envelope, "SERVER_REQUEST_RESPONSE_REJECTED")?;
                send_protocol_error(downstream, rpc_id, "REQUEST_NOT_PENDING").await?;
                tracing::warn!(
                    worker_id = %config.worker_id,
                    connection_epoch,
                    proxy_seq,
                    error = %error,
                    "blocked invalid TUI server-request response"
                );
                return Ok(());
            }
        };
        if let Err(error) = upstream.send(Message::Text(text.to_string().into())).await {
            if let Some(boundary) = boundary.as_ref() {
                let _ = config
                    .event_sink
                    .outcome_unknown(boundary, "UPSTREAM_DISCONNECTED");
            }
            return Err(error.into());
        }
        pending_server_requests.remove(&request_id);
        if let Some(boundary) = boundary.as_ref() {
            if let Err(error) = config.event_sink.upstream_write_completed(boundary) {
                let _ = config
                    .event_sink
                    .outcome_unknown(boundary, "WRITE_COMMIT_FAILED");
                return Err(error);
            }
            if let Err(error) = config
                .event_sink
                .upstream_response_committed(boundary, &json!({"result":{"requestId":request_id}}))
            {
                let _ = config
                    .event_sink
                    .outcome_unknown(boundary, "RESPONSE_COMMIT_FAILED");
                return Err(error);
            }
        }
        return Ok(());
    };
    let Some(original_id) = value.get("id").cloned() else {
        let classification = classify_method(&method);
        let current_owner = owner.read().expect("proxy owner poisoned").clone();
        let boundary = match config.event_sink.prepare_upstream_write(
            &envelope,
            &method,
            classification,
            current_owner.as_ref(),
        ) {
            Ok(boundary) => boundary,
            Err(error) if classification != ProtocolClassification::KnownReadOnly => {
                config
                    .event_sink
                    .protocol_anomaly(&envelope, "INPUT_ATTRIBUTION_UNKNOWN")?;
                tracing::warn!(
                    worker_id = %config.worker_id,
                    connection_epoch,
                    proxy_seq,
                    method,
                    error = %error,
                    "blocked unattributed TUI protocol notification"
                );
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if let Err(error) = upstream.send(Message::Text(text.to_string().into())).await {
            if let Some(boundary) = boundary.as_ref() {
                let _ = config
                    .event_sink
                    .outcome_unknown(boundary, "UPSTREAM_DISCONNECTED");
            }
            return Err(error.into());
        }
        if let Some(boundary) = boundary {
            if let Err(error) = config.event_sink.upstream_write_completed(&boundary) {
                let _ = config
                    .event_sink
                    .outcome_unknown(&boundary, "WRITE_COMMIT_FAILED");
                return Err(error);
            }
            pending_notifications.push(boundary);
        }
        return Ok(());
    };
    let classification = classify_method(&method);
    let current_owner = owner.read().expect("proxy owner poisoned").clone();
    let boundary = match config.event_sink.prepare_upstream_write(
        &envelope,
        &method,
        classification,
        current_owner.as_ref(),
    ) {
        Ok(boundary) => boundary,
        Err(error) if classification != ProtocolClassification::KnownReadOnly => {
            downstream
                .send(Message::Text(
                    json!({
                        "id": original_id,
                        "error": {"code": -32050, "message": "INPUT_ATTRIBUTION_UNKNOWN"}
                    })
                    .to_string()
                    .into(),
                ))
                .await?;
            config
                .event_sink
                .protocol_anomaly(&envelope, "INPUT_ATTRIBUTION_UNKNOWN")?;
            tracing::warn!(
                worker_id = %config.worker_id,
                connection_epoch,
                proxy_seq,
                method,
                error = %error,
                "blocked unattributed TUI protocol mutation"
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let upstream_id = ids.map_tui_request(
        original_id,
        boundary
            .as_ref()
            .map(|boundary| boundary.command_id.clone()),
    );
    value["id"] = upstream_id;
    upstream
        .send(Message::Text(serde_json::to_string(&value)?.into()))
        .await?;
    if let Some(boundary) = boundary.as_ref() {
        config.event_sink.upstream_write_completed(boundary)?;
    }
    Ok(())
}

async fn handle_upstream_text(
    text: &str,
    config: &ProxyConfig,
    position: ProxyPosition<'_>,
    ids: &mut RequestIdMap,
    downstream: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
    pending_controls: &mut std::collections::HashMap<String, PendingProxyControl>,
    pending_server_requests: &mut std::collections::HashMap<String, PendingServerRequest>,
) -> Result<()> {
    let ProxyPosition {
        connection_epoch,
        proxy_seq,
    } = position;
    let mut value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            let envelope = proxy_envelope(
                config,
                connection_epoch,
                proxy_seq,
                ProxyDirection::UpstreamToTui,
                Value::Null,
            );
            config
                .event_sink
                .commit_decode_error(&envelope, text, "INVALID_JSON")?;
            config
                .event_sink
                .protocol_anomaly(&envelope, "INVALID_JSON")?;
            tracing::warn!(
                worker_id = %config.worker_id,
                connection_epoch,
                proxy_seq,
                error = %error,
                "ignored recoverable invalid upstream protocol frame"
            );
            return Ok(());
        }
    };
    let envelope = proxy_envelope(
        config,
        connection_epoch,
        proxy_seq,
        ProxyDirection::UpstreamToTui,
        value.clone(),
    );
    config.event_sink.commit_envelope(&envelope)?;
    if let Err(error) = validate_app_server_envelope(&value) {
        config
            .event_sink
            .protocol_anomaly(&envelope, "INVALID_ENVELOPE")?;
        tracing::warn!(
            worker_id = %config.worker_id,
            connection_epoch,
            proxy_seq,
            error = %error,
            "ignored recoverable invalid upstream protocol envelope"
        );
        return Ok(());
    }

    if value.get("method").is_none() {
        let upstream_id = value.get("id").cloned().context("response lacks id")?;
        if let Some(control_id) = upstream_id.as_str()
            && let Some(pending) = pending_controls.remove(control_id)
        {
            config
                .event_sink
                .upstream_response_committed(&pending.boundary, &value)?;
            let _ = pending.reply.send(Ok(value));
            return Ok(());
        }
        let Some(mapped) = ids.take_tui_response(&upstream_id) else {
            config
                .event_sink
                .protocol_anomaly(&envelope, "UNKNOWN_RESPONSE_ID")?;
            return Ok(());
        };
        if let Some(command_id) = mapped.command_id {
            config
                .event_sink
                .upstream_response_committed(&MutationBoundary { command_id }, &value)?;
        }
        value["id"] = mapped.original_id;
        downstream
            .send(Message::Text(serde_json::to_string(&value)?.into()))
            .await?;
    } else if value.get("id").is_some() {
        let route = config.event_sink.server_request_route(&envelope)?;
        match route {
            ServerRequestRoute::Terminal => {
                let rpc_id = value.get("id").cloned().unwrap_or(Value::Null);
                let request_id = serde_json::to_string(&rpc_id)?;
                let method = value
                    .get("method")
                    .and_then(Value::as_str)
                    .context("server request lacks method")?
                    .to_string();
                let payload = value.get("params").cloned().unwrap_or(Value::Null);
                pending_server_requests.insert(
                    request_id,
                    PendingServerRequest {
                        rpc_id,
                        method,
                        payload,
                        value: value.clone(),
                        route: ServerRequestRoute::Terminal,
                    },
                );
                downstream
                    .send(Message::Text(text.to_string().into()))
                    .await?;
            }
            route @ ServerRequestRoute::Channel { .. } => {
                let rpc_id = value.get("id").cloned().unwrap_or(Value::Null);
                let request_id = serde_json::to_string(&rpc_id)?;
                let method = value
                    .get("method")
                    .and_then(Value::as_str)
                    .context("server request lacks method")?
                    .to_string();
                let payload = value.get("params").cloned().unwrap_or(Value::Null);
                pending_server_requests.insert(
                    request_id,
                    PendingServerRequest {
                        rpc_id,
                        method,
                        payload,
                        value: value.clone(),
                        route,
                    },
                );
            }
            ServerRequestRoute::Unattributed => {
                config
                    .event_sink
                    .protocol_anomaly(&envelope, "REQUEST_OWNER_UNATTRIBUTED")?;
                let rpc_id = value.get("id").cloned().unwrap_or(Value::Null);
                let request_id = serde_json::to_string(&rpc_id)?;
                let method = value
                    .get("method")
                    .and_then(Value::as_str)
                    .context("server request lacks method")?
                    .to_string();
                let payload = value.get("params").cloned().unwrap_or(Value::Null);
                pending_server_requests.insert(
                    request_id,
                    PendingServerRequest {
                        rpc_id,
                        method,
                        payload,
                        value,
                        route: ServerRequestRoute::Unattributed,
                    },
                );
            }
        }
    } else {
        downstream
            .send(Message::Text(text.to_string().into()))
            .await?;
    }
    Ok(())
}

async fn send_protocol_error(
    downstream: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
    id: Value,
    code: &'static str,
) -> Result<()> {
    downstream
        .send(Message::Text(
            json!({"id":id,"error":{"code":-32700,"message":code}})
                .to_string()
                .into(),
        ))
        .await?;
    Ok(())
}

fn proxy_envelope(
    config: &ProxyConfig,
    connection_epoch: &str,
    proxy_seq: u64,
    direction: ProxyDirection,
    value: Value,
) -> ProxyEnvelope {
    ProxyEnvelope {
        source_id: config.source_id.clone(),
        store_source_id: config.store_source_id.clone(),
        source_epoch: config.source_epoch.clone(),
        worker_id: config.worker_id.clone(),
        connection_epoch: connection_epoch.into(),
        proxy_seq,
        direction,
        value,
    }
}

fn classify_method(method: &str) -> ProtocolClassification {
    if matches!(
        method,
        "initialize"
            | "initialized"
            | "model/list"
            | "permissionProfile/list"
            | "mcpServerStatus/list"
            | "account/usage/read"
            | "account/rateLimits/read"
            | "collaborationMode/list"
            | "thread/list"
            | "thread/read"
            | "thread/loaded/list"
            | "thread/goal/get"
    ) {
        ProtocolClassification::KnownReadOnly
    } else if matches!(
        method,
        "thread/start"
            | "thread/resume"
            | "thread/fork"
            | "thread/name/set"
            | "thread/archive"
            | "thread/compact/start"
            | "review/start"
            | "thread/goal/set"
            | "thread/goal/clear"
            | "thread/settings/update"
            | "turn/start"
            | "turn/steer"
            | "turn/interrupt"
    ) {
        ProtocolClassification::KnownMutation
    } else {
        ProtocolClassification::UnknownPossibleMutation
    }
}

fn validate_private_socket_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("private proxy socket has no parent")?;
    let metadata = std::fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("private proxy parent must be a direct directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            anyhow::bail!("private proxy parent must be owned by the current user with mode 0700");
        }
    }
    if path.exists() {
        anyhow::bail!("private proxy socket already exists");
    }
    Ok(())
}

fn validate_peer(stream: &UnixStream, expected_pid: u32) -> Result<()> {
    let credentials = stream
        .peer_cred()
        .context("read private proxy peer identity")?;
    if credentials.uid() != unsafe { libc::geteuid() } {
        anyhow::bail!("private proxy peer is not owned by the current user");
    }
    let peer_pid = credentials
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .context("private proxy peer PID is unavailable")?;
    if peer_pid != expected_pid {
        anyhow::bail!("private proxy peer is not the authorized Session Worker child");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
