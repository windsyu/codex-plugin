use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;

use crate::session::{
    CreateFakeSession, CreateSession, ExpectedActiveTurn, InputLeaseView, OutputEvent,
    SessionError, SessionWorkerHandle, TerminalSnapshot,
};

use super::auth::issue_session;
use super::{ApiState, AuditPrincipal, mutation_origin_allowed, v2_error, v2_error_details};

const TERMINAL_PROTOCOL: &str = "codex-terminal-v1";
const DESCRIPTOR_PROTOCOL_PREFIX: &str = "codex-attach.";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CreateFakeSessionRequest {
    cwd: String,
    rows: Option<u16>,
    cols: Option<u16>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CreateSessionRequest {
    store_source_id: String,
    source_id: String,
    source_epoch: String,
    expected_supervisor_version: u64,
    mode: String,
    codex_thread_id: Option<String>,
    cwd: String,
    rows: Option<u16>,
    cols: Option<u16>,
}

pub(super) async fn session_sources(State(state): State<ApiState>) -> Response {
    let registry = match state.session_kernel.registry() {
        Ok(registry) => registry,
        Err(error) => return session_error_response(error),
    };
    Json(json!({"apiVersion":"v2","data":registry.session_sources()})).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AttachSessionRequest {
    resume_attachment_id: Option<String>,
    resume_attachment_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct InputLeaseRequest {
    attachment_id: String,
    attachment_token: String,
    expected_version: u64,
    #[serde(default)]
    takeover: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ReleaseInputLeaseRequest {
    attachment_id: String,
    attachment_token: String,
    expected_version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct InterruptSessionRequest {
    source_epoch: String,
    thread_id: String,
    expected_turn_id: String,
    expected_worker_version: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StopSessionRequest {
    source_epoch: Option<String>,
    expected_worker_version: Option<i64>,
    active_turn_policy: Option<String>,
    #[serde(default)]
    expected_active_turns: Vec<StopExpectedActiveTurn>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StopExpectedActiveTurn {
    thread_id: String,
    turn_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum TerminalClientFrame {
    Input { lease_id: String, data: String },
    Resize { cols: u16, rows: u16 },
    Ack { output_seq: u64 },
    RequestSnapshot { after_seq: Option<u64> },
}

#[derive(Debug, Clone, Copy)]
enum MutationHeaderError {
    OriginRejected,
    IdempotencyKeyRequired,
}

impl MutationHeaderError {
    fn response(self) -> Response {
        match self {
            Self::OriginRejected => v2_error(
                StatusCode::FORBIDDEN,
                "ORIGIN_REJECTED",
                "session mutation requests require an allowed Origin",
                None,
            ),
            Self::IdempotencyKeyRequired => v2_error(
                StatusCode::BAD_REQUEST,
                "IDEMPOTENCY_KEY_REQUIRED",
                "Idempotency-Key must contain 8 to 200 ASCII characters",
                None,
            ),
        }
    }
}

pub(super) async fn create_fake_session(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    headers: HeaderMap,
    Json(request): Json<CreateFakeSessionRequest>,
) -> Response {
    let idempotency_key = match mutation_headers(&state, &headers) {
        Ok(key) => key,
        Err(error) => return error.response(),
    };
    let registry = match state.session_kernel.registry() {
        Ok(registry) => registry,
        Err(error) => return session_error_response(error),
    };
    let request = CreateFakeSession {
        cwd: request.cwd.into(),
        rows: request.rows.unwrap_or(24),
        cols: request.cols.unwrap_or(80),
    };
    match registry
        .create_fake(principal.0, idempotency_key, request)
        .await
    {
        Ok(outcome) => (
            if outcome.replayed {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            },
            Json(json!({
                "apiVersion":"v2",
                "data":outcome.snapshot,
                "idempotentReplay":outcome.replayed
            })),
        )
            .into_response(),
        Err(error) => session_error_response(error),
    }
}

pub(super) async fn create_session(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    headers: HeaderMap,
    Json(request): Json<CreateSessionRequest>,
) -> Response {
    let idempotency_key = match mutation_headers(&state, &headers) {
        Ok(key) => key,
        Err(error) => return error.response(),
    };
    let registry = match state.session_kernel.registry() {
        Ok(registry) => registry,
        Err(error) => return session_error_response(error),
    };
    match registry
        .create(
            principal.0,
            idempotency_key,
            CreateSession {
                store_source_id: request.store_source_id,
                source_id: request.source_id,
                source_epoch: request.source_epoch,
                expected_supervisor_version: request.expected_supervisor_version,
                mode: request.mode,
                codex_thread_id: request.codex_thread_id,
                cwd: request.cwd.into(),
                rows: request.rows.unwrap_or(24),
                cols: request.cols.unwrap_or(80),
            },
        )
        .await
    {
        Ok(outcome) => {
            let view = match registry.view(&outcome.snapshot.worker_id.0) {
                Ok(view) => view,
                Err(error) => return session_error_response(error),
            };
            (
                if outcome.replayed || outcome.existing_owner {
                    StatusCode::OK
                } else {
                    StatusCode::CREATED
                },
                Json(json!({
                    "apiVersion":"v2",
                    "data":view,
                    "idempotentReplay":outcome.replayed,
                    "existingOwner":outcome.existing_owner
                })),
            )
                .into_response()
        }
        Err(error) => session_error_response(error),
    }
}

pub(super) async fn get_session(
    State(state): State<ApiState>,
    Path(worker_id): Path<String>,
) -> Response {
    let registry = match state.session_kernel.registry() {
        Ok(registry) => registry,
        Err(error) => return session_error_response(error),
    };
    match registry.view(&worker_id) {
        Ok(view) => Json(json!({"apiVersion":"v2","data":view})).into_response(),
        Err(error) => session_error_response(error),
    }
}

pub(super) async fn session_events(
    State(state): State<ApiState>,
    Path(worker_id): Path<String>,
) -> Response {
    let registry = match state.session_kernel.registry() {
        Ok(registry) => registry.clone(),
        Err(error) => return session_error_response(error),
    };
    let worker = match registry.get(&worker_id) {
        Some(worker) => worker,
        None => {
            return session_error_response(SessionError {
                code: "SESSION_NOT_FOUND",
                message: "Session Worker not found".into(),
            });
        }
    };
    let mut state_changes = worker.subscribe_state();
    let stream = async_stream::stream! {
        let mut last_payload = String::new();
        let mut refresh = tokio::time::interval(Duration::from_secs(1));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let view = match registry.view(&worker_id) {
                Ok(view) => view,
                Err(error) => {
                    yield Ok::<Event, Infallible>(Event::default().event("error").json_data(json!({
                        "code":error.code,
                        "message":error.message,
                    })).expect("session error event is serializable"));
                    break;
                }
            };
            let payload = serde_json::to_string(&view).expect("SessionView is serializable");
            if payload != last_payload {
                let event_id = view.persisted_worker.as_ref()
                    .map(|worker| worker.version.to_string())
                    .unwrap_or_else(|| view.snapshot.output_seq.to_string());
                yield Ok::<Event, Infallible>(Event::default()
                    .id(event_id)
                    .event("session_state")
                    .data(payload.clone()));
                last_payload = payload;
            }
            if view.snapshot.state.is_terminal() {
                break;
            }
            tokio::select! {
                changed = state_changes.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                _ = refresh.tick() => {}
            }
        }
    };
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("session-heartbeat"),
        )
        .into_response()
}

pub(super) async fn attach_session(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    Path(worker_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<AttachSessionRequest>,
) -> Response {
    if let Err(error) = mutation_headers(&state, &headers) {
        return error.response();
    }
    let worker = match session_worker(&state, &worker_id) {
        Ok(worker) => worker,
        Err(error) => return session_error_response(error),
    };
    let bearer_auth = principal.0 == "local_bearer";
    match worker
        .prepare_attachment(
            terminal_principal(&principal),
            request.resume_attachment_id,
            request.resume_attachment_token,
        )
        .await
    {
        Ok(descriptor) => {
            let mut response = (
                [(header::CACHE_CONTROL, "no-store")],
                Json(json!({"apiVersion":"v2","data":descriptor})),
            )
                .into_response();
            if bearer_auth
                && let Ok(session) = issue_session(&state.token, chrono::Utc::now().timestamp())
                && let Ok(cookie) = format!(
                    "observer_session={session}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000"
                )
                .parse()
            {
                response.headers_mut().insert(header::SET_COOKIE, cookie);
            }
            response
        }
        Err(error) => session_error_response(error),
    }
}

pub(super) async fn acquire_input_lease(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    Path(worker_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<InputLeaseRequest>,
) -> Response {
    if let Err(error) = mutation_headers(&state, &headers) {
        return error.response();
    }
    let worker = match session_worker(&state, &worker_id) {
        Ok(worker) => worker,
        Err(error) => return session_error_response(error),
    };
    match worker
        .acquire_input_lease(
            terminal_principal(&principal),
            request.attachment_id,
            request.attachment_token,
            request.expected_version,
            request.takeover,
        )
        .await
    {
        Ok(lease) => input_lease_response(lease),
        Err(error) => session_error_response(error),
    }
}

pub(super) async fn release_input_lease(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    Path((worker_id, lease_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<ReleaseInputLeaseRequest>,
) -> Response {
    if let Err(error) = mutation_headers(&state, &headers) {
        return error.response();
    }
    let worker = match session_worker(&state, &worker_id) {
        Ok(worker) => worker,
        Err(error) => return session_error_response(error),
    };
    match worker
        .release_input_lease(
            terminal_principal(&principal),
            request.attachment_id,
            request.attachment_token,
            lease_id,
            request.expected_version,
        )
        .await
    {
        Ok(lease) => input_lease_response(lease),
        Err(error) => session_error_response(error),
    }
}

pub(super) async fn stop_session(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    Path(worker_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<StopSessionRequest>,
) -> Response {
    let idempotency_key = match mutation_headers(&state, &headers) {
        Ok(key) => key,
        Err(error) => return error.response(),
    };
    let registry = match state.session_kernel.registry() {
        Ok(registry) => registry,
        Err(error) => return session_error_response(error),
    };
    let persisted = match registry.view(&worker_id) {
        Ok(view) => view.persisted_worker,
        Err(error) => return session_error_response(error),
    };
    if persisted.is_none() {
        let worker = match session_worker(&state, &worker_id) {
            Ok(worker) => worker,
            Err(error) => return session_error_response(error),
        };
        return match worker.stop().await {
            Ok(snapshot) => Json(json!({"apiVersion":"v2","data":snapshot})).into_response(),
            Err(error) => session_error_response(error),
        };
    }
    let (Some(source_epoch), Some(expected_worker_version), Some(active_turn_policy)) = (
        request.source_epoch,
        request.expected_worker_version,
        request.active_turn_policy,
    ) else {
        return v2_error(
            StatusCode::BAD_REQUEST,
            "STOP_PRECONDITION_REQUIRED",
            "persistent Session stop requires sourceEpoch, expectedWorkerVersion and activeTurnPolicy",
            None,
        );
    };
    match registry
        .stop_session(
            principal.0,
            idempotency_key,
            &worker_id,
            &source_epoch,
            expected_worker_version,
            &active_turn_policy,
            request
                .expected_active_turns
                .into_iter()
                .map(|turn| ExpectedActiveTurn {
                    thread_id: turn.thread_id,
                    turn_id: turn.turn_id,
                })
                .collect(),
        )
        .await
    {
        Ok(outcome) => Json(json!({
            "apiVersion":"v2",
            "data":outcome.snapshot,
            "command":outcome.command
        }))
        .into_response(),
        Err(error) => session_error_response(error),
    }
}

pub(super) async fn interrupt_session(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    Path(worker_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<InterruptSessionRequest>,
) -> Response {
    let idempotency_key = match mutation_headers(&state, &headers) {
        Ok(key) => key,
        Err(error) => return error.response(),
    };
    let registry = match state.session_kernel.registry() {
        Ok(registry) => registry,
        Err(error) => return session_error_response(error),
    };
    match registry
        .interrupt(
            principal.0,
            idempotency_key,
            &worker_id,
            &request.source_epoch,
            &request.thread_id,
            &request.expected_turn_id,
            request.expected_worker_version,
        )
        .await
    {
        Ok(command) => Json(json!({"apiVersion":"v2","data":command})).into_response(),
        Err(error) => session_error_response(error),
    }
}

pub(super) async fn terminal_socket(
    ws: WebSocketUpgrade,
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    Path(worker_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !mutation_origin_allowed(&state, &headers) {
        return v2_error(
            StatusCode::FORBIDDEN,
            "ORIGIN_REJECTED",
            "terminal WebSocket requires an allowed Origin",
            None,
        );
    }
    let Some(descriptor) = attachment_descriptor(&headers) else {
        return v2_error(
            StatusCode::UNAUTHORIZED,
            "ATTACHMENT_DESCRIPTOR_REQUIRED",
            "terminal WebSocket requires an attachment descriptor",
            None,
        );
    };
    let worker = match session_worker(&state, &worker_id) {
        Ok(worker) => worker,
        Err(error) => return session_error_response(error),
    };
    let attachment_id = match worker
        .connect_attachment(terminal_principal(&principal), descriptor.to_owned())
        .await
    {
        Ok(attachment_id) => attachment_id,
        Err(error) => return session_error_response(error),
    };
    ws.protocols([TERMINAL_PROTOCOL])
        .max_frame_size(64 * 1024)
        .max_message_size(64 * 1024)
        .on_upgrade(move |socket| handle_terminal_socket(socket, worker, attachment_id))
        .into_response()
}

async fn handle_terminal_socket(
    mut socket: WebSocket,
    worker: SessionWorkerHandle,
    attachment_id: String,
) {
    let mut output = worker.subscribe_output();
    let mut state = worker.subscribe_state();
    let snapshot = match worker.terminal_snapshot(None).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = send_error(&mut socket, &error).await;
            worker.disconnect_attachment(attachment_id).await;
            return;
        }
    };
    let mut delivered_seq = snapshot.to_seq;
    let initial_state = state.borrow().clone();
    if !send_snapshot(&mut socket, &snapshot).await
        || !send_state(&mut socket, &initial_state).await
    {
        worker.disconnect_attachment(attachment_id).await;
        return;
    }
    let mut frame_window = Instant::now();
    let mut frames_in_window = 0_u32;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.tick().await;
    loop {
        tokio::select! {
            event = output.recv() => {
                match event {
                    Ok(event) if event.output_seq > delivered_seq => {
                        delivered_seq = event.output_seq;
                        if !send_output(&mut socket, &event).await { break; }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let error = SessionError { code:"SLOW_CONSUMER", message:"terminal consumer fell behind the bounded output stream".into() };
                        let _ = send_error(&mut socket, &error).await;
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            changed = state.changed() => {
                if changed.is_err() { break; }
                let current_state = state.borrow().clone();
                if !send_state(&mut socket, &current_state).await { break; }
            }
            incoming = socket.next() => {
                let Some(incoming) = incoming else { break; };
                let Ok(message) = incoming else { break; };
                if frame_window.elapsed() >= Duration::from_secs(1) {
                    frame_window = Instant::now();
                    frames_in_window = 0;
                }
                frames_in_window = frames_in_window.saturating_add(1);
                if frames_in_window > 200 {
                    let error = SessionError { code:"TERMINAL_FRAME_FLOOD", message:"terminal frame rate exceeded the supported limit".into() };
                    let _ = send_error(&mut socket, &error).await;
                    break;
                }
                if let Some(error) = unsupported_terminal_client_message(&message) {
                    let _ = send_error(&mut socket, &error).await;
                    break;
                }
                match message {
                    Message::Text(text) => {
                        let frame = match serde_json::from_str::<TerminalClientFrame>(&text) {
                            Ok(frame) => frame,
                            Err(_) => {
                                let error = SessionError { code:"TERMINAL_FRAME_INVALID", message:"terminal client frame is invalid".into() };
                                let _ = send_error(&mut socket, &error).await;
                                break;
                            }
                        };
                        let result = handle_client_frame(&worker, &attachment_id, frame).await;
                        match result {
                            Ok(Some(snapshot)) => {
                                delivered_seq = snapshot.to_seq;
                                if !send_snapshot(&mut socket, &snapshot).await { break; }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                if !send_error(&mut socket, &error).await { break; }
                            }
                        }
                    }
                    Message::Ping(value) => {
                        if socket.send(Message::Pong(value)).await.is_err() { break; }
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => break,
                    Message::Binary(_) => unreachable!("binary messages are rejected before dispatch"),
                }
            }
            _ = heartbeat.tick() => {
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(2), socket.send(Message::Ping(Vec::new().into()))).await,
                    Ok(Ok(()))
                ) { break; }
            }
        }
    }
    worker.disconnect_attachment(attachment_id).await;
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.close()).await;
}

fn unsupported_terminal_client_message(message: &Message) -> Option<SessionError> {
    matches!(message, Message::Binary(_)).then(|| SessionError {
        code: "TERMINAL_FRAME_INVALID",
        message: "terminal client frames must use typed JSON text".into(),
    })
}

async fn handle_client_frame(
    worker: &SessionWorkerHandle,
    attachment_id: &str,
    frame: TerminalClientFrame,
) -> Result<Option<TerminalSnapshot>, SessionError> {
    match frame {
        TerminalClientFrame::Input { lease_id, data } => {
            worker
                .write_input(attachment_id.into(), lease_id, data.into_bytes())
                .await?;
            Ok(None)
        }
        TerminalClientFrame::Resize { cols, rows } => {
            worker.resize(attachment_id.into(), rows, cols).await?;
            Ok(None)
        }
        TerminalClientFrame::Ack { output_seq } => {
            worker.acknowledge(attachment_id.into(), output_seq).await;
            Ok(None)
        }
        TerminalClientFrame::RequestSnapshot { after_seq } => {
            worker.terminal_snapshot(after_seq).await.map(Some)
        }
    }
}

fn session_worker(state: &ApiState, worker_id: &str) -> Result<SessionWorkerHandle, SessionError> {
    let registry = state.session_kernel.registry()?;
    registry.get(worker_id).ok_or_else(|| SessionError {
        code: "SESSION_NOT_FOUND",
        message: "Session Worker was not found".into(),
    })
}

fn mutation_headers(state: &ApiState, headers: &HeaderMap) -> Result<String, MutationHeaderError> {
    if !mutation_origin_allowed(state, headers) {
        return Err(MutationHeaderError::OriginRejected);
    }
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| (8..=200).contains(&value.len()) && value.is_ascii())
        .map(str::to_owned)
        .ok_or(MutationHeaderError::IdempotencyKeyRequired)
}

fn terminal_principal(principal: &AuditPrincipal) -> String {
    if matches!(principal.0.as_str(), "local_bearer" | "local_cookie") {
        "local_session".into()
    } else {
        principal.0.clone()
    }
}

fn attachment_descriptor(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .find_map(|protocol| protocol.strip_prefix(DESCRIPTOR_PROTOCOL_PREFIX))
        .filter(|descriptor| {
            (32..=160).contains(&descriptor.len())
                && descriptor
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        })
}

fn input_lease_response(lease: InputLeaseView) -> Response {
    Json(json!({"apiVersion":"v2","data":lease})).into_response()
}

pub(super) fn session_error_response(error: SessionError) -> Response {
    let status = match error.code {
        "CAPABILITY_UNAVAILABLE"
        | "SESSION_KERNEL_CLI_UNAVAILABLE"
        | "SESSION_PROXY_UNAVAILABLE" => StatusCode::SERVICE_UNAVAILABLE,
        "SESSION_NOT_FOUND" | "ATTACHMENT_NOT_FOUND" => StatusCode::NOT_FOUND,
        "INPUT_LEASE_CONFLICT"
        | "IDEMPOTENCY_CONFLICT"
        | "TURN_STATE_CONFLICT"
        | "WORKER_VERSION_CONFLICT"
        | "SUPERVISOR_VERSION_STALE"
        | "SOURCE_EPOCH_STALE"
        | "ATTACHMENT_ALREADY_CONNECTED"
        | "ATTACHMENT_EXPIRED" => StatusCode::CONFLICT,
        "SESSION_WORKER_BUSY" | "ATTACHMENT_LIMIT_REACHED" => StatusCode::TOO_MANY_REQUESTS,
        "SESSION_WORKER_EXITED" => StatusCode::GONE,
        "ATTACHMENT_DESCRIPTOR_INVALID"
        | "ATTACHMENT_DESCRIPTOR_EXPIRED"
        | "ATTACHMENT_PRINCIPAL_MISMATCH"
        | "ATTACHMENT_TOKEN_INVALID"
        | "ATTACHMENT_TOKEN_REQUIRED" => StatusCode::UNAUTHORIZED,
        "SESSION_WORKER_SPAWN_FAILED" | "SESSION_WORKER_STOP_FAILED" => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
        _ => StatusCode::BAD_REQUEST,
    };
    v2_error_details(
        status,
        error.code,
        &error.message,
        json!({"retryable":matches!(status,StatusCode::TOO_MANY_REQUESTS|StatusCode::SERVICE_UNAVAILABLE)}),
    )
}

async fn send_output(socket: &mut WebSocket, event: &OutputEvent) -> bool {
    let mut frame = Vec::with_capacity(9 + event.data.len());
    frame.push(1);
    frame.extend_from_slice(&event.output_seq.to_be_bytes());
    frame.extend_from_slice(&event.data);
    matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            socket.send(Message::Binary(frame.into()))
        )
        .await,
        Ok(Ok(()))
    )
}

async fn send_snapshot(socket: &mut WebSocket, snapshot: &TerminalSnapshot) -> bool {
    send_json(
        socket,
        json!({
            "type":"snapshot",
            "checkpointSeq":snapshot.checkpoint_seq,
            "fromSeq":snapshot.from_seq,
            "toSeq":snapshot.to_seq,
            "rows":snapshot.rows,
            "cols":snapshot.cols,
            "screen":STANDARD.encode(&snapshot.screen),
            "replay":STANDARD.encode(&snapshot.replay),
            "encoding":"base64",
            "complete":snapshot.complete,
            "truncated":snapshot.truncated
        }),
    )
    .await
}

async fn send_state(socket: &mut WebSocket, state: &crate::session::WorkerSnapshot) -> bool {
    send_json(socket, json!({"type":"state","worker":state})).await
}

async fn send_error(socket: &mut WebSocket, error: &SessionError) -> bool {
    send_json(
        socket,
        json!({"type":"error","code":error.code,"message":error.message}),
    )
    .await
}

async fn send_json(socket: &mut WebSocket, value: serde_json::Value) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            socket.send(Message::Text(value.to_string().into()))
        )
        .await,
        Ok(Ok(()))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::middleware as axum_middleware;
    use axum::routing::{get, post};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use tempfile::TempDir;
    use tower::ServiceExt;

    use crate::controller::ControllerRegistry;
    use crate::session::SessionKernel;
    use crate::store::Database;
    use crate::writer::WriterHandle;

    #[cfg(unix)]
    fn fake_cli(temp: &TempDir) -> anyhow::Result<std::path::PathBuf> {
        let path = temp.path().join("fake-codex");
        fs::write(
            &path,
            b"#!/bin/sh\nprintf 'SESSION_READY\\r\\n'\nwhile IFS= read -r line; do printf 'ECHO:%s\\r\\n' \"$line\"; done\n",
        )?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(path)
    }

    fn test_state(temp: &TempDir, session_kernel: SessionKernel) -> anyhow::Result<ApiState> {
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        Ok(ApiState {
            writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
            controller: Some(ControllerRegistry::default()),
            session_kernel,
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([7_u8; 32])),
            fingerprint_key: [7; 32],
            strict_origin: true,
            allowed_origins: Arc::new(vec!["http://127.0.0.1:4765".into()]),
            live_modes: Arc::new(Vec::new()),
            blob_downloads: Arc::new(tokio::sync::Semaphore::new(1)),
            settings: Arc::new(json!({"controller":{"enabled":true}})),
            tailscale: None,
        })
    }

    fn test_app(state: ApiState) -> Router {
        Router::new()
            .route("/v2/session-sources", get(session_sources))
            .route("/v2/sessions", post(create_session))
            .route("/v2/sessions/fake", post(create_fake_session))
            .route("/v2/sessions/{worker_id}", get(get_session))
            .route("/v2/sessions/{worker_id}/events", get(session_events))
            .route("/v2/sessions/{worker_id}/attach", post(attach_session))
            .route(
                "/v2/sessions/{worker_id}/input-lease",
                post(acquire_input_lease),
            )
            .route("/v2/sessions/{worker_id}/terminal", get(terminal_socket))
            .route(
                "/v2/sessions/{worker_id}/interrupt",
                post(interrupt_session),
            )
            .route_layer(axum_middleware::from_fn_with_state(
                state.clone(),
                super::super::authorize,
            ))
            .with_state(state)
    }

    async fn response_json(response: Response) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX).await?,
        )?)
    }

    #[test]
    fn attachment_descriptor_is_header_only_bounded_and_typed() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            "codex-terminal-v1, codex-attach.abc_DEF-012345678901234567890123456789"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            attachment_descriptor(&headers),
            Some("abc_DEF-012345678901234567890123456789")
        );
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            "codex-attach.invalid/descriptor".parse().unwrap(),
        );
        assert_eq!(attachment_descriptor(&headers), None);
    }

    #[test]
    fn typed_client_frames_reject_unknown_and_binary_shaped_payloads() {
        assert!(
            serde_json::from_str::<TerminalClientFrame>(r#"{"type":"resize","rows":24,"cols":80}"#)
                .is_ok()
        );
        assert!(
            serde_json::from_str::<TerminalClientFrame>(
                r#"{"type":"resize","rows":24,"cols":80,"rawMethod":"turn/start"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<TerminalClientFrame>(r#"{"type":"raw","method":"turn/start"}"#)
                .is_err()
        );
    }

    #[test]
    fn typed_client_frames_accept_the_frozen_browser_camel_case_contract() {
        match serde_json::from_str::<TerminalClientFrame>(
            r#"{"type":"input","leaseId":"lease-1","data":"hello"}"#,
        )
        .expect("browser input frame")
        {
            TerminalClientFrame::Input { lease_id, data } => {
                assert_eq!(lease_id, "lease-1");
                assert_eq!(data, "hello");
            }
            _ => panic!("expected input frame"),
        }

        match serde_json::from_str::<TerminalClientFrame>(r#"{"type":"ack","outputSeq":42}"#)
            .expect("browser ack frame")
        {
            TerminalClientFrame::Ack { output_seq } => assert_eq!(output_seq, 42),
            _ => panic!("expected ack frame"),
        }

        match serde_json::from_str::<TerminalClientFrame>(
            r#"{"type":"requestSnapshot","afterSeq":41}"#,
        )
        .expect("browser snapshot frame")
        {
            TerminalClientFrame::RequestSnapshot { after_seq } => {
                assert_eq!(after_seq, Some(41));
            }
            _ => panic!("expected snapshot frame"),
        }

        assert!(
            serde_json::from_str::<TerminalClientFrame>(r#"{"type":"ack","output_seq":42}"#)
                .is_err(),
            "snake_case fields are not part of the browser wire contract"
        );
    }

    #[test]
    fn websocket_pong_is_transport_control_not_a_typed_frame_error() {
        assert!(unsupported_terminal_client_message(&Message::Pong(Vec::new().into())).is_none());
        assert_eq!(
            unsupported_terminal_client_message(&Message::Binary(Vec::new().into()))
                .expect("client binary frame must remain rejected")
                .code,
            "TERMINAL_FRAME_INVALID"
        );
    }

    #[test]
    fn persistent_stop_contract_is_closed_and_versioned() {
        let request = serde_json::from_str::<StopSessionRequest>(
            r#"{
                "sourceEpoch":"epoch-1",
                "expectedWorkerVersion":7,
                "activeTurnPolicy":"interrupt_expected",
                "expectedActiveTurns":[{"threadId":"thread-1","turnId":"turn-1"}]
            }"#,
        )
        .expect("valid stop contract");
        assert_eq!(request.source_epoch.as_deref(), Some("epoch-1"));
        assert_eq!(request.expected_worker_version, Some(7));
        assert_eq!(request.expected_active_turns.len(), 1);
        assert!(
            serde_json::from_str::<StopSessionRequest>(
                r#"{"activeTurnPolicy":"reject_if_active","rawMethod":"turn/interrupt"}"#
            )
            .is_err(),
            "stop must not expose a raw protocol escape hatch"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_routes_are_default_off_origin_bound_and_idempotent() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let token = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let disabled = test_app(test_state(&temp, SessionKernel::disabled())?);
        let response = disabled
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v2/sessions/fake")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::ORIGIN, "http://127.0.0.1:4765")
                    .header("idempotency-key", "session-disabled-key")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"cwd":{:?},"rows":24,"cols":80}}"#,
                        temp.path().to_string_lossy()
                    )))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response_json(response).await?["error"]["code"],
            "CAPABILITY_UNAVAILABLE"
        );

        let fixture = fake_cli(&temp)?;
        let kernel = SessionKernel::preview_for_test(temp.path().join("runtime"), fixture)?;
        let preview = test_app(test_state(&temp, kernel.clone())?);
        let body = json!({"cwd":temp.path(),"rows":24,"cols":80}).to_string();
        let cross_origin = preview
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v2/sessions/fake")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::ORIGIN, "https://evil.example")
                    .header("idempotency-key", "session-preview-key")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.clone()))?,
            )
            .await?;
        assert_eq!(cross_origin.status(), StatusCode::FORBIDDEN);

        let cross_origin_terminal = preview
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v2/sessions/not-created/terminal")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::ORIGIN, "https://evil.example")
                    .header(header::CONNECTION, "upgrade")
                    .header(header::UPGRADE, "websocket")
                    .header("sec-websocket-version", "13")
                    .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(cross_origin_terminal.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response_json(cross_origin_terminal).await?["error"]["code"],
            "ORIGIN_REJECTED"
        );

        let create = |key: &'static str| {
            Request::builder()
                .method("POST")
                .uri("/v2/sessions/fake")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::ORIGIN, "http://127.0.0.1:4765")
                .header("idempotency-key", key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.clone()))
        };
        let created = preview
            .clone()
            .oneshot(create("session-preview-key")?)
            .await?;
        assert_eq!(created.status(), StatusCode::CREATED);
        let created = response_json(created).await?;
        let worker_id = created["data"]["workerId"].as_str().unwrap().to_owned();
        let events = preview
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v2/sessions/{worker_id}/events"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(events.status(), StatusCode::OK);
        assert_eq!(events.headers()[header::CONTENT_TYPE], "text/event-stream");
        let replay = preview
            .clone()
            .oneshot(create("session-preview-key")?)
            .await?;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(response_json(replay).await?["data"]["workerId"], worker_id);

        let attach = preview
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v2/sessions/{worker_id}/attach"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::ORIGIN, "http://127.0.0.1:4765")
                    .header("idempotency-key", "session-attach-key")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"resumeAttachmentId":null,"resumeAttachmentToken":null}"#,
                    ))?,
            )
            .await?;
        assert_eq!(attach.status(), StatusCode::OK);
        assert!(attach.headers().get(header::SET_COOKIE).is_some());
        assert_eq!(attach.headers()[header::CACHE_CONTROL], "no-store");
        let attach = response_json(attach).await?;
        let attachment_id = attach["data"]["attachmentId"].as_str().unwrap();
        let attachment_token = attach["data"]["attachmentToken"].as_str().unwrap();
        assert_eq!(attachment_token.len(), 64);
        assert!(attach["data"]["descriptor"].as_str().is_some());

        let forged_lease = preview
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v2/sessions/{worker_id}/input-lease"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::ORIGIN, "http://127.0.0.1:4765")
                    .header("idempotency-key", "session-lease-forged-key")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "attachmentId":attachment_id,
                            "attachmentToken":"0".repeat(64),
                            "expectedVersion":0,
                            "takeover":false
                        })
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(forged_lease.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response_json(forged_lease).await?["error"]["code"],
            "ATTACHMENT_TOKEN_INVALID"
        );

        let incomplete_resume = preview
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v2/sessions/{worker_id}/attach"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::ORIGIN, "http://127.0.0.1:4765")
                    .header("idempotency-key", "session-resume-incomplete-key")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "resumeAttachmentId":attachment_id,
                            "resumeAttachmentToken":null
                        })
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(incomplete_resume.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response_json(incomplete_resume).await?["error"]["code"],
            "ATTACHMENT_TOKEN_REQUIRED"
        );
        kernel.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn real_session_create_rejects_stale_supervisor_version_before_spawning()
    -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let token = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let database = Arc::new(Database::open(&temp.path().join("runtime.sqlite"))?);
        database.migrate()?;
        let writer = WriterHandle::start(database.clone(), 4096, 16, 512)?;
        let codex_home = temp.path().join("codex-home");
        fs::create_dir(&codex_home)?;
        let kernel = SessionKernel::runtime_for_test(
            temp.path().join("runtime"),
            fake_cli(&temp)?,
            vec![crate::session::SessionSource {
                store_source_id: "store-real".into(),
                source_id: "source-real".into(),
                source_epoch: "epoch-current".into(),
                supervisor_version: 2,
                codex_home,
                default_cwd: temp.path().to_path_buf(),
                test_upstream_socket: None,
            }],
            writer,
            database,
        )?;
        let state = test_state(&temp, kernel)?;
        let app = test_app(state);
        let sources = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v2/session-sources")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(sources.status(), StatusCode::OK);
        let sources = response_json(sources).await?;
        assert_eq!(sources["data"][0]["storeSourceId"], "store-real");
        assert_eq!(sources["data"][0]["sourceId"], "source-real");
        assert_eq!(sources["data"][0]["sourceEpoch"], "epoch-current");
        assert_eq!(sources["data"][0]["supervisorVersion"], 2);
        assert_eq!(sources["data"][0]["status"], "ready");
        assert!(sources["data"][0].get("codexHome").is_none());
        assert!(sources["data"][0].get("socket").is_none());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v2/sessions")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::ORIGIN, "http://127.0.0.1:4765")
                    .header("idempotency-key", "session-stale-supervisor")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "storeSourceId":"store-real",
                            "sourceId":"source-real",
                            "sourceEpoch":"epoch-current",
                            "expectedSupervisorVersion":1,
                            "mode":"new",
                            "codexThreadId":null,
                            "cwd":temp.path(),
                            "rows":24,
                            "cols":80
                        })
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = response_json(response).await?;
        assert_eq!(body["error"]["code"], "SUPERVISOR_VERSION_STALE");

        let stale_epoch = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v2/sessions")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::ORIGIN, "http://127.0.0.1:4765")
                    .header("idempotency-key", "session-stale-epoch")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "storeSourceId":"store-real",
                            "sourceId":"source-real",
                            "sourceEpoch":"epoch-old",
                            "expectedSupervisorVersion":2,
                            "mode":"new",
                            "codexThreadId":null,
                            "cwd":temp.path()
                        })
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(stale_epoch.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(stale_epoch).await?["error"]["code"],
            "SOURCE_EPOCH_STALE"
        );
        Ok(())
    }
}
