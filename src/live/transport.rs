use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{WebSocketStream, client_async};
use uuid::Uuid;

use crate::clock::now_ms;
use crate::config::{Config, SourceConfig};
use crate::controller::{
    ActorCommand, ActorRequest, CapabilityCatalog, ControllerOperation, ControllerRegistry,
    EXPERIMENTAL_API_ENABLED, EXPERIMENTAL_CATALOG_METHODS, PendingRequestAction,
    SERVER_REQUEST_METHODS, STABLE_CATALOG_METHODS, SlashCommandEntry, SourceActorSnapshot,
    SourceControlCatalog, ThreadSetting, actor_channel, catalog_request_params,
};
use crate::credentials::load_or_create_key;
#[cfg(test)]
use crate::domain::gateway::GatewayCommandOrigin;
use crate::domain::gateway::{GatewayCommandRecord, GatewayTransition};
use crate::domain::identity::{app_server_source_id, stable_source_id, thread_key};
use crate::domain::live::{
    classify_live_item, live_summary, validate_app_server_envelope as validate_envelope,
};
use crate::domain::model::{NormalizedEvent, OwnedIngestBatch};
use crate::domain::redact;
use crate::domain::session::SourceEpochStale;
use crate::permissions::validate_private_unix_socket;
use crate::store::Database;
use crate::store::PendingRequestClaim;
use crate::writer::WriterHandle;

#[cfg(not(test))]
const COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_millis(100);

trait LiveIngest {
    fn ingest_live(&self, batch: OwnedIngestBatch) -> Result<(usize, usize)>;
}

impl LiveIngest for Database {
    fn ingest_live(&self, batch: OwnedIngestBatch) -> Result<(usize, usize)> {
        self.ingest_batch(&batch)
    }
}

impl LiveIngest for WriterHandle {
    fn ingest_live(&self, batch: OwnedIngestBatch) -> Result<(usize, usize)> {
        self.ingest(batch)
    }
}

struct LiveSession {
    app_source_id: String,
    store_source_id: String,
    epoch_id: String,
    socket_path: PathBuf,
    source_seq: i64,
    fingerprint_key: [u8; 32],
    attached_threads: BTreeSet<String>,
    active_turns: BTreeMap<String, String>,
    thread_statuses: BTreeMap<String, String>,
    thread_models: BTreeMap<String, String>,
    thread_reasoning_efforts: BTreeMap<String, Value>,
    thread_collaboration_modes: BTreeMap<String, Value>,
    thread_default_collaboration_modes: BTreeMap<String, Value>,
    thread_goals: BTreeMap<String, Value>,
    pending_requests: BTreeMap<String, PendingServerRequest>,
    turn_commands: BTreeMap<String, String>,
    terminal_turns: BTreeMap<String, String>,
    dispatching_turn_command: Option<(String, String)>,
    reconciled_threads: BTreeMap<String, String>,
    rpc_request_id: i64,
    controller_enabled: bool,
    capability_catalog: CapabilityCatalog,
    keep_reasoning: bool,
    keep_raw_json: bool,
}

#[derive(Debug, Clone)]
struct PendingServerRequest {
    rpc_id: Value,
    method: String,
    thread_id: String,
    version: i64,
    payload: Value,
}

impl PendingServerRequest {
    fn response_for(
        &self,
        action: &PendingRequestAction,
    ) -> std::result::Result<(Value, &'static str), (&'static str, &'static str)> {
        match (self.method.as_str(), action) {
            (
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval",
                PendingRequestAction::Approval { decision },
            ) => {
                if !matches!(
                    decision.as_str(),
                    "accept" | "acceptForSession" | "decline" | "cancel"
                ) {
                    return Err(("COMMAND_INVALID", "approval decision is invalid"));
                }
                if self.method == "item/commandExecution/requestApproval"
                    && self
                        .payload
                        .get("availableDecisions")
                        .and_then(Value::as_array)
                        .is_some_and(|available| {
                            !available
                                .iter()
                                .any(|value| value.as_str() == Some(decision))
                        })
                {
                    return Err((
                        "CAPABILITY_UNAVAILABLE",
                        "approval decision is not offered by the source",
                    ));
                }
                Ok((json!({"decision":decision}), "approval"))
            }
            (
                "item/permissions/requestApproval",
                PendingRequestAction::Permissions {
                    grant,
                    scope,
                    strict_auto_review,
                },
            ) => {
                if !matches!(scope.as_str(), "turn" | "session") {
                    return Err(("COMMAND_INVALID", "permission grant scope is invalid"));
                }
                let permissions = if *grant {
                    self.payload
                        .get("permissions")
                        .cloned()
                        .unwrap_or_else(|| json!({}))
                } else {
                    json!({})
                };
                Ok((
                    json!({
                        "permissions":permissions,
                        "scope":scope,
                        "strictAutoReview":strict_auto_review,
                    }),
                    "permissions",
                ))
            }
            ("item/tool/requestUserInput", PendingRequestAction::UserInput { answers }) => {
                let questions = self
                    .payload
                    .get("questions")
                    .and_then(Value::as_array)
                    .ok_or((
                        "UPSTREAM_PROTOCOL_INVALID",
                        "question request lacks questions",
                    ))?;
                if answers.len() != questions.len() {
                    return Err(("COMMAND_INVALID", "every question requires one answer"));
                }
                for question in questions {
                    let id = question
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or(("UPSTREAM_PROTOCOL_INVALID", "question request lacks an id"))?;
                    let values = answers
                        .get(id)
                        .ok_or(("COMMAND_INVALID", "question answer id is invalid"))?;
                    if values.is_empty() || values.len() > 20 {
                        return Err(("COMMAND_INVALID", "question answer count is invalid"));
                    }
                    if let Some(options) = question.get("options").and_then(Value::as_array)
                        && !question
                            .get("isOther")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        && values.iter().any(|answer| {
                            !options.iter().any(|option| {
                                option.get("label").and_then(Value::as_str) == Some(answer)
                            })
                        })
                    {
                        return Err((
                            "COMMAND_INVALID",
                            "question answer is not an advertised option",
                        ));
                    }
                }
                let answers = answers
                    .iter()
                    .map(|(id, values)| (id.clone(), json!({"answers":values})))
                    .collect::<serde_json::Map<_, _>>();
                Ok((json!({"answers":answers}), "user_input"))
            }
            (
                "mcpServer/elicitation/request",
                PendingRequestAction::McpElicitation { action, content },
            ) => {
                if !matches!(action.as_str(), "accept" | "decline" | "cancel") {
                    return Err(("COMMAND_INVALID", "MCP elicitation action is invalid"));
                }
                if action == "accept"
                    && self.payload.get("mode").and_then(Value::as_str) != Some("url")
                {
                    let schema = self.payload.get("requestedSchema").ok_or((
                        "UPSTREAM_PROTOCOL_INVALID",
                        "MCP elicitation request lacks a schema",
                    ))?;
                    let content = content
                        .as_ref()
                        .ok_or(("COMMAND_INVALID", "accepted MCP elicitation needs content"))?;
                    validate_mcp_content(schema, content)?;
                } else if action != "accept" && content.is_some() {
                    return Err((
                        "COMMAND_INVALID",
                        "declined MCP elicitation must not contain content",
                    ));
                }
                Ok((
                    json!({"action":action,"content":content}),
                    "mcp_elicitation",
                ))
            }
            _ => Err((
                "COMMAND_INVALID",
                "request action does not match the pending request type",
            )),
        }
    }
}

fn validate_mcp_content(
    schema: &Value,
    content: &Value,
) -> std::result::Result<(), (&'static str, &'static str)> {
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err((
            "UPSTREAM_PROTOCOL_INVALID",
            "MCP elicitation schema is not an object schema",
        ));
    }
    let object = content.as_object().ok_or((
        "COMMAND_INVALID",
        "MCP elicitation content must be an object",
    ))?;
    let properties = schema.get("properties").and_then(Value::as_object).ok_or((
        "UPSTREAM_PROTOCOL_INVALID",
        "MCP elicitation schema is invalid",
    ))?;
    if object.keys().any(|key| !properties.contains_key(key)) {
        return Err((
            "COMMAND_INVALID",
            "MCP elicitation content has an unknown field",
        ));
    }
    if schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|required| {
            required
                .iter()
                .filter_map(Value::as_str)
                .any(|key| !object.contains_key(key))
        })
    {
        return Err((
            "COMMAND_INVALID",
            "MCP elicitation content lacks a required field",
        ));
    }
    for (key, value) in object {
        let field = &properties[key];
        let valid = match field.get("type").and_then(Value::as_str) {
            Some("string") => value.is_string(),
            Some("number") => value.is_number(),
            Some("boolean") => value.is_boolean(),
            Some("array") => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
            _ => false,
        };
        if !valid {
            return Err(("COMMAND_INVALID", "MCP elicitation content type is invalid"));
        }
        if let Some(text) = value.as_str() {
            let length = text.chars().count() as u64;
            if field
                .get("minLength")
                .and_then(Value::as_u64)
                .is_some_and(|minimum| length < minimum)
                || field
                    .get("maxLength")
                    .and_then(Value::as_u64)
                    .is_some_and(|maximum| length > maximum)
            {
                return Err((
                    "COMMAND_INVALID",
                    "MCP elicitation string length is invalid",
                ));
            }
        }
        if let Some(number) = value.as_f64()
            && (field
                .get("minimum")
                .and_then(Value::as_f64)
                .is_some_and(|minimum| number < minimum)
                || field
                    .get("maximum")
                    .and_then(Value::as_f64)
                    .is_some_and(|maximum| number > maximum))
        {
            return Err((
                "COMMAND_INVALID",
                "MCP elicitation number is outside the allowed range",
            ));
        }
        if let Some(values) = value.as_array()
            && (field
                .get("minItems")
                .and_then(Value::as_u64)
                .is_some_and(|minimum| values.len() < minimum as usize)
                || field
                    .get("maxItems")
                    .and_then(Value::as_u64)
                    .is_some_and(|maximum| values.len() > maximum as usize))
        {
            return Err((
                "COMMAND_INVALID",
                "MCP elicitation selection count is invalid",
            ));
        }
        if let Some(allowed) = mcp_allowed_values(field) {
            let values = value
                .as_array()
                .map(|values| values.iter().collect::<Vec<_>>())
                .unwrap_or_else(|| vec![value]);
            if values.iter().any(|value| !allowed.contains(value)) {
                return Err(("COMMAND_INVALID", "MCP elicitation value is not allowed"));
            }
        }
    }
    Ok(())
}

fn mcp_allowed_values(field: &Value) -> Option<Vec<&Value>> {
    if let Some(values) = field.get("enum").and_then(Value::as_array) {
        return Some(values.iter().collect());
    }
    if let Some(values) = field.get("oneOf").and_then(Value::as_array) {
        return Some(
            values
                .iter()
                .filter_map(|value| value.get("const"))
                .collect(),
        );
    }
    if let Some(values) = field.pointer("/items/enum").and_then(Value::as_array) {
        return Some(values.iter().collect());
    }
    field
        .pointer("/items/anyOf")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.get("const"))
                .collect()
        })
}

pub struct LiveRuntime {
    pub controller: ControllerRegistry,
    pub handles: Vec<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
struct ActorRuntimeConfig {
    registry: ControllerRegistry,
    database: Arc<Database>,
    source_stale_sender: Option<mpsc::UnboundedSender<SourceEpochStale>>,
    controller_enabled: bool,
    fingerprint_key: [u8; 32],
    keep_reasoning: bool,
    keep_raw_json: bool,
}

pub fn spawn_enabled(
    config: &Config,
    database: Arc<Database>,
    writer: WriterHandle,
    source_stale_sender: Option<mpsc::UnboundedSender<SourceEpochStale>>,
    shutdown: watch::Receiver<bool>,
) -> Result<LiveRuntime> {
    let fingerprint_key = load_or_create_key(&config.storage.fingerprint_key_file)?;
    let controller = ControllerRegistry::default();
    let mut handles = Vec::new();
    for source in config
        .sources
        .iter()
        .filter(|source| {
            source.app_server_socket.is_some()
                && (source.live_mode != "off" || config.controller.enabled)
        })
        .cloned()
    {
        let runtime = ActorRuntimeConfig {
            registry: controller.clone(),
            database: database.clone(),
            source_stale_sender: source_stale_sender.clone(),
            controller_enabled: config.controller.enabled,
            fingerprint_key,
            keep_reasoning: config.capture.keep_reasoning,
            keep_raw_json: config.capture.keep_raw_json,
        };
        let writer = writer.clone();
        let shutdown = shutdown.clone();
        handles.push(tokio::spawn(async move {
            run_with_reconnect(source, writer, runtime, shutdown).await;
        }));
    }
    Ok(LiveRuntime {
        controller,
        handles,
    })
}

pub async fn doctor_probe(source: &SourceConfig) -> String {
    let Some(socket_path) = source.app_server_socket.as_ref() else {
        return "not_configured".into();
    };
    let probe = async {
        validate_socket(socket_path)?;
        let stream = UnixStream::connect(socket_path).await?;
        let (mut websocket, _) = client_async("ws://localhost/", stream).await?;
        websocket.send(Message::Text(json!({"method":"initialize","id":1,
            "params":{"clientInfo":{"name":"codex_local_observer_doctor","title":"Codex Local Observer Doctor","version":env!("CARGO_PKG_VERSION")},
            "capabilities":{"experimentalApi":false}}}).to_string().into())).await?;
        let message = websocket
            .next()
            .await
            .context("app-server closed during initialize probe")??;
        let Message::Text(text) = message else {
            bail!("initialize probe returned non-text response");
        };
        let envelope: Value = serde_json::from_str(&text)?;
        validate_envelope(&envelope)?;
        let home = envelope
            .get("result")
            .and_then(|result| result.get("codexHome"))
            .and_then(Value::as_str)
            .context("initialize result lacks codexHome")?;
        if canonical_existing(Path::new(home))? != canonical_existing(&source.codex_home)? {
            bail!("initialize codexHome mismatch");
        }
        Result::<()>::Ok(())
    };
    match tokio::time::timeout(Duration::from_secs(5), probe).await {
        Ok(Ok(())) => "initialize_compatible".into(),
        Ok(Err(error)) => format!(
            "incompatible_or_unreachable: {}",
            sanitize_probe_error(&error)
        ),
        Err(_) => "incompatible_or_unreachable: timeout".into(),
    }
}

fn sanitize_probe_error(error: &anyhow::Error) -> String {
    let message = error.to_string();
    if message.contains("codexHome") {
        "codexHome mismatch".into()
    } else if message.contains("incompatible protocol") {
        "protocol envelope incompatible".into()
    } else {
        "connection or initialize failed".into()
    }
}

async fn run_with_reconnect(
    source: SourceConfig,
    writer: WriterHandle,
    runtime: ActorRuntimeConfig,
    mut shutdown: watch::Receiver<bool>,
) {
    let socket_path = source
        .app_server_socket
        .clone()
        .expect("validated live source has a socket");
    let app_source_id = app_server_source_id(&socket_path);
    let stable_identity = socket_path.to_string_lossy().to_string();
    if let Err(error) = writer.upsert_source_kind(
        &app_source_id,
        "app_server",
        &stable_identity,
        &json!({"name":source.name,"socket":stable_identity,"liveMode":source.live_mode,
            "controllerEnabled":runtime.controller_enabled}),
        "discovering",
    ) {
        tracing::error!(source_id = %app_source_id, error = %error, "register live source failed");
        return;
    }

    let mut backoff_ms = 500_u64;
    loop {
        if *shutdown.borrow() {
            let _ = writer.update_source_status(&app_source_id, "paused", None);
            return;
        }
        let epoch_id = Uuid::new_v4().to_string();
        let (actor_sender, actor_receiver) = actor_channel();
        let result = connect_once(
            &source,
            &writer,
            &runtime.registry,
            &runtime.database,
            (actor_sender, actor_receiver),
            LiveSession {
                app_source_id: app_source_id.clone(),
                store_source_id: stable_source_id(&source.codex_home.to_string_lossy()),
                epoch_id: epoch_id.clone(),
                socket_path: socket_path.clone(),
                source_seq: 0,
                fingerprint_key: runtime.fingerprint_key,
                attached_threads: BTreeSet::new(),
                active_turns: BTreeMap::new(),
                thread_statuses: BTreeMap::new(),
                thread_models: BTreeMap::new(),
                thread_reasoning_efforts: BTreeMap::new(),
                thread_collaboration_modes: BTreeMap::new(),
                thread_default_collaboration_modes: BTreeMap::new(),
                thread_goals: BTreeMap::new(),
                pending_requests: BTreeMap::new(),
                turn_commands: BTreeMap::new(),
                terminal_turns: BTreeMap::new(),
                dispatching_turn_command: None,
                reconciled_threads: BTreeMap::new(),
                rpc_request_id: 100,
                controller_enabled: runtime.controller_enabled,
                capability_catalog: CapabilityCatalog::default(),
                keep_reasoning: runtime.keep_reasoning,
                keep_raw_json: runtime.keep_raw_json,
            },
            shutdown.clone(),
        )
        .await;
        if *shutdown.borrow() {
            runtime
                .registry
                .mark_unavailable(&app_source_id, &epoch_id, "graceful_shutdown");
            let _ = writer.close_live_epoch(&app_source_id, &epoch_id, "graceful_shutdown");
            let _ = writer.update_source_status(&app_source_id, "paused", None);
            return;
        }
        let reason = result
            .as_ref()
            .err()
            .map(|error| format!("{error:#}"))
            .unwrap_or_else(|| "connection_closed".into());
        if let Some(source_stale_sender) = &runtime.source_stale_sender {
            let _ = source_stale_sender.send(SourceEpochStale {
                source_id: app_source_id.clone(),
                source_epoch: epoch_id.clone(),
            });
        }
        runtime
            .registry
            .mark_unavailable(&app_source_id, &epoch_id, &reason);
        if reason.contains("incompatible protocol:") {
            let _ = writer.close_live_epoch(&app_source_id, &epoch_id, "incompatible");
            let _ = writer.update_source_status(&app_source_id, "incompatible", Some(&reason));
            tracing::error!(source_id = %app_source_id, error = %reason, "live protocol incompatible; store-only fallback active until restart");
            return;
        }
        if let Err(error) = writer.close_live_epoch(&app_source_id, &epoch_id, &reason) {
            tracing::warn!(source_id = %app_source_id, error = %error, "close live epoch failed");
        }
        let _ = writer.update_source_status(&app_source_id, "degraded", Some(&reason));
        tracing::warn!(source_id = %app_source_id, error = %reason, "live source disconnected");
        let delay = rand::rng().random_range(0..=backoff_ms);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(delay)) => {}
            _ = wait_for_shutdown(&mut shutdown) => return,
        }
        backoff_ms = (backoff_ms * 2).min(30_000);
    }
}

async fn connect_once(
    source: &SourceConfig,
    writer: &WriterHandle,
    registry: &ControllerRegistry,
    database: &Database,
    actor_channel: (mpsc::Sender<ActorRequest>, mpsc::Receiver<ActorRequest>),
    mut session: LiveSession,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let (actor_sender, mut actor_receiver) = actor_channel;
    validate_socket(&session.socket_path)?;
    writer.update_source_status(&session.app_source_id, "connecting", None)?;
    let stream = UnixStream::connect(&session.socket_path)
        .await
        .with_context(|| {
            format!(
                "connect app-server socket {}",
                session.socket_path.display()
            )
        })?;
    let (mut websocket, _) = client_async("ws://localhost/", stream)
        .await
        .context("upgrade app-server Unix socket to WebSocket")?;

    websocket
        .send(Message::Text(
            json!({
                "method":"initialize","id":1,
                "params":{"clientInfo":{"name":"codex_local_gateway","title":"Codex Local Gateway","version":env!("CARGO_PKG_VERSION")},
                "capabilities":{"experimentalApi":session.controller_enabled && EXPERIMENTAL_API_ENABLED}}
            })
            .to_string()
            .into(),
        ))
        .await?;
    let initialize = wait_for_response(
        &mut websocket,
        writer,
        &mut session,
        json!(1),
        "initialize/response",
        None,
    )
    .await?;
    let result = initialize.get("result").ok_or_else(|| {
        anyhow::anyhow!("incompatible protocol: initialize response is missing result")
    })?;
    let returned_home = result
        .get("codexHome")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!("incompatible protocol: initialize response is missing codexHome")
        })?;
    if canonical_existing(Path::new(returned_home))? != canonical_existing(&source.codex_home)? {
        bail!("app-server codexHome does not match configured source");
    }
    websocket
        .send(Message::Text(
            json!({"method":"initialized"}).to_string().into(),
        ))
        .await?;
    if session.controller_enabled {
        session.capability_catalog =
            detect_capability_catalog(&mut websocket, writer, &mut session, source).await?;
        discover_loaded_threads(&mut websocket, writer, &mut session).await?;
        writer.record_live_capabilities(
            &session.app_source_id,
            &session.epoch_id,
            &json!({
                "initialize":result,
                "controller":{
                    "experimentalApi":EXPERIMENTAL_API_ENABLED,
                    "catalog":session.capability_catalog.summary()
                }
            }),
        )?;
        registry.publish(session.snapshot("ready"), actor_sender.clone());
    } else {
        writer.record_live_capabilities(&session.app_source_id, &session.epoch_id, result)?;
    }
    writer.update_source_status(&session.app_source_id, "ready", None)?;
    tracing::info!(source_id = %session.app_source_id, mode = %source.live_mode, "live source initialized");

    if source.live_mode != "off" {
        reconcile_threads(&mut websocket, writer, &mut session).await?;
        if source.live_mode == "attach_loaded" {
            attach_loaded_threads(&mut websocket, writer, database, &mut session).await?;
        }
    }

    let mut reconcile =
        tokio::time::interval(Duration::from_secs(source.scan_interval_seconds.max(30)));
    reconcile.tick().await;

    let actor_result: Result<()> = async {
        loop {
            let message = tokio::select! {
                _ = wait_for_shutdown(&mut shutdown) => {
                    unsubscribe_attached(&mut websocket, &mut session).await?;
                    return Ok(());
                }
                message = websocket.next() => message,
                request = actor_receiver.recv(), if session.controller_enabled => {
                    let Some(request) = request else {
                        bail!("live source actor command channel closed");
                    };
                    match request {
                        ActorRequest::Snapshot { reply } => {
                            let _ = reply.send(session.snapshot("ready"));
                        }
                        ActorRequest::Dispatch { command, reply } => {
                            let (record, close_source) = dispatch_actor_command(
                                &mut websocket,
                                writer,
                                database,
                                &mut session,
                                command,
                            ).await?;
                            let _ = reply.send(record);
                            if close_source {
                                bail!("live source transport became unavailable during command dispatch");
                            }
                        }
                        ActorRequest::Catalog {
                            thread_id,
                            thread_key: supplied_thread_key,
                            reply,
                        } => {
                            let verified_thread_id = thread_id.as_deref().filter(|thread_id| {
                                supplied_thread_key.as_deref()
                                    == Some(thread_key(&session.store_source_id, thread_id).as_str())
                            });
                            let _ = reply.send(session.control_catalog(verified_thread_id));
                        }
                    }
                    continue;
                }
                _ = reconcile.tick() => {
                    detach_worker_owned_threads(
                        &mut websocket,
                        writer,
                        database,
                        &mut session,
                    ).await?;
                    if source.live_mode != "off" {
                        reconcile_threads(&mut websocket, writer, &mut session).await?;
                        if source.live_mode == "attach_loaded" {
                            attach_loaded_threads(
                                &mut websocket,
                                writer,
                                database,
                                &mut session,
                            ).await?;
                        }
                    }
                    continue;
                }
            };
            let Some(message) = message else { break };
            match message? {
                Message::Text(text) => {
                    let envelope: Value = serde_json::from_str(&text)?;
                    let observation = ingest_envelope(writer, &mut session, &envelope, None, None)?;
                    let idle_thread_id = (observation.method == "thread/status/changed"
                        && observation.thread_status.as_deref() == Some("idle"))
                    .then(|| observation.thread_id.clone())
                    .flatten();
                    apply_control_observation(writer, &mut session, observation)?;
                    if let Some(thread_id) = idle_thread_id {
                        reconcile_idle_thread_turn(
                            &mut websocket,
                            writer,
                            &mut session,
                            &thread_id,
                        )
                        .await?;
                    }
                }
                Message::Close(_) => break,
                Message::Ping(payload) => websocket.send(Message::Pong(payload)).await?,
                Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }
        Ok(())
    }
    .await;
    let close_reason = if *shutdown.borrow() {
        "GATEWAY_SHUTDOWN"
    } else {
        "SOURCE_DISCONNECTED"
    };
    mark_inflight_outcome_unknown(writer, &mut session, close_reason);
    actor_result
}

impl LiveSession {
    fn snapshot(&self, state: &str) -> SourceActorSnapshot {
        SourceActorSnapshot {
            source_id: self.app_source_id.clone(),
            source_epoch: self.epoch_id.clone(),
            supervisor_version: 1,
            state: state.into(),
            experimental_api: self.controller_enabled && EXPERIMENTAL_API_ENABLED,
            catalog: self.capability_catalog.clone(),
            unavailable_reason: None,
        }
    }

    fn control_catalog(&self, thread_id: Option<&str>) -> SourceControlCatalog {
        let thread_loaded = thread_id.is_some_and(|id| self.attached_threads.contains(id));
        let active_turn_id = thread_id.and_then(|id| self.active_turns.get(id)).cloned();
        let mut slash_commands = vec![SlashCommandEntry {
            name: "/new".into(),
            capability: "thread.start".into(),
            interaction_required_without_argument: true,
        }];
        if thread_id.is_some() {
            if !thread_loaded {
                slash_commands.push(SlashCommandEntry {
                    name: "/resume".into(),
                    capability: "thread.resume".into(),
                    interaction_required_without_argument: false,
                });
            }
            slash_commands.push(SlashCommandEntry {
                name: "/fork".into(),
                capability: "thread.fork".into(),
                interaction_required_without_argument: false,
            });
            for (name, capability, interaction, method) in [
                ("/rename", "thread.name.set", true, Some("thread/name/set")),
                ("/archive", "thread.archive", false, Some("thread/archive")),
                ("/status", "status.read", false, None),
            ] {
                if method.is_some_and(|method| !self.method_available(method)) {
                    continue;
                }
                slash_commands.push(SlashCommandEntry {
                    name: name.into(),
                    capability: capability.into(),
                    interaction_required_without_argument: interaction,
                });
            }
            if self
                .capability_catalog
                .entries
                .get("mcpServerStatus/list")
                .is_some_and(|entry| entry.available)
            {
                slash_commands.push(SlashCommandEntry {
                    name: "/mcp".into(),
                    capability: "mcp.status.read".into(),
                    interaction_required_without_argument: false,
                });
            }
            if ["account/usage/read", "account/rateLimits/read"]
                .iter()
                .all(|method| {
                    self.capability_catalog
                        .entries
                        .get(*method)
                        .is_some_and(|entry| entry.available)
                })
            {
                slash_commands.push(SlashCommandEntry {
                    name: "/usage".into(),
                    capability: "account.usage.read".into(),
                    interaction_required_without_argument: false,
                });
            }
        }
        if thread_loaded {
            if active_turn_id.is_none() {
                slash_commands.push(SlashCommandEntry {
                    name: "/clear".into(),
                    capability: "thread.clear".into(),
                    interaction_required_without_argument: false,
                });
            }
            if self.method_available("thread/settings/update") {
                for (name, capability) in [
                    ("/model", "thread.settings.model"),
                    ("/reasoning", "thread.settings.reasoning"),
                    ("/permissions", "thread.settings.permissions"),
                ] {
                    slash_commands.push(SlashCommandEntry {
                        name: name.into(),
                        capability: capability.into(),
                        interaction_required_without_argument: true,
                    });
                }
            }
            if self
                .capability_catalog
                .entries
                .get("thread/goal/get")
                .is_none_or(|entry| entry.available)
            {
                slash_commands.push(SlashCommandEntry {
                    name: "/goal".into(),
                    capability: "thread.goal".into(),
                    interaction_required_without_argument: false,
                });
            }
            if thread_id.is_some_and(|id| {
                self.method_available("thread/settings/update")
                    && self
                        .capability_catalog
                        .collaboration_mode(
                            "plan",
                            self.thread_models.get(id).map(String::as_str),
                            self.thread_reasoning_efforts.get(id),
                        )
                        .is_some()
                    && self
                        .capability_catalog
                        .collaboration_mode(
                            "default",
                            self.thread_models.get(id).map(String::as_str),
                            self.thread_reasoning_efforts.get(id),
                        )
                        .is_some()
            }) {
                slash_commands.push(SlashCommandEntry {
                    name: "/plan".into(),
                    capability: "thread.plan".into(),
                    interaction_required_without_argument: false,
                });
            }
            if active_turn_id.is_none() {
                for (name, capability, method) in [
                    ("/compact", "thread.compact", "thread/compact/start"),
                    ("/review", "review.start", "review/start"),
                ] {
                    if !self.method_available(method) {
                        continue;
                    }
                    slash_commands.push(SlashCommandEntry {
                        name: name.into(),
                        capability: capability.into(),
                        interaction_required_without_argument: false,
                    });
                }
            }
            if self.method_available("thread/settings/update")
                && thread_id
                    .and_then(|id| self.thread_models.get(id))
                    .is_some_and(|model| self.capability_catalog.model_supports_personality(model))
            {
                slash_commands.push(SlashCommandEntry {
                    name: "/personality".into(),
                    capability: "thread.settings.personality".into(),
                    interaction_required_without_argument: true,
                });
            }
        }
        if active_turn_id.is_some() {
            slash_commands.push(SlashCommandEntry {
                name: "/interrupt".into(),
                capability: "turn.interrupt".into(),
                interaction_required_without_argument: false,
            });
        }
        SourceControlCatalog {
            source_id: self.app_source_id.clone(),
            source_epoch: self.epoch_id.clone(),
            thread_loaded,
            active_turn_id,
            collaboration_mode: thread_id
                .and_then(|id| self.thread_collaboration_modes.get(id))
                .cloned(),
            goal: thread_id.and_then(|id| self.thread_goals.get(id)).cloned(),
            capabilities: self.capability_catalog.clone(),
            slash_commands,
        }
    }

    fn method_available(&self, method: &str) -> bool {
        self.capability_catalog
            .entries
            .get(method)
            .is_none_or(|entry| entry.available)
    }
}

async fn detect_capability_catalog(
    websocket: &mut WebSocketStream<UnixStream>,
    writer: &WriterHandle,
    session: &mut LiveSession,
    source: &SourceConfig,
) -> Result<CapabilityCatalog> {
    let mut catalog = CapabilityCatalog::default();
    for (method, experimental) in STABLE_CATALOG_METHODS
        .iter()
        .map(|method| (*method, false))
        .chain(
            EXPERIMENTAL_CATALOG_METHODS
                .iter()
                .map(|method| (*method, true)),
        )
    {
        session.rpc_request_id += 1;
        let request_id = session.rpc_request_id;
        let params = catalog_request_params(method, &source.codex_home)
            .expect("catalog method is fixed by protocol manifest");
        websocket
            .send(Message::Text(
                json!({"method":method,"id":request_id,"params":params})
                    .to_string()
                    .into(),
            ))
            .await?;
        let response_method = format!("{method}/response");
        let response = wait_for_correlated_response(
            websocket,
            writer,
            session,
            json!(request_id),
            &response_method,
            None,
        )
        .await?;
        catalog.record_response(method, experimental, &response);
    }
    Ok(catalog)
}

const THREAD_SOURCE_KINDS: &[&str] = &[
    "cli",
    "vscode",
    "exec",
    "appServer",
    "subAgent",
    "subAgentReview",
    "subAgentCompact",
    "subAgentThreadSpawn",
    "subAgentOther",
    "unknown",
];

async fn reconcile_threads(
    websocket: &mut WebSocketStream<UnixStream>,
    writer: &WriterHandle,
    session: &mut LiveSession,
) -> Result<()> {
    for archived in [false, true] {
        let mut cursor: Option<String> = None;
        loop {
            session.rpc_request_id += 1;
            let request_id = session.rpc_request_id;
            websocket.send(Message::Text(json!({"method":"thread/list","id":request_id,
                "params":{"cursor":cursor,"limit":200,"archived":archived,"sourceKinds":THREAD_SOURCE_KINDS}}).to_string().into())).await?;
            let response = wait_for_response(
                websocket,
                writer,
                session,
                json!(request_id),
                "thread/list/response",
                None,
            )
            .await?;
            let result = response.get("result").ok_or_else(|| {
                anyhow::anyhow!("incompatible protocol: thread/list response is missing result")
            })?;
            let data = result
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "incompatible protocol: thread/list result.data is not an array"
                    )
                })?;
            let mut reads = Vec::new();
            for thread in data {
                let thread_id = thread.get("id").and_then(Value::as_str).ok_or_else(|| {
                    anyhow::anyhow!("incompatible protocol: thread/list item lacks id")
                })?;
                let version = thread
                    .get("updatedAt")
                    .map(Value::to_string)
                    .unwrap_or_else(|| thread.to_string());
                if session.reconciled_threads.get(thread_id) != Some(&version) {
                    reads.push((thread_id.to_string(), version));
                }
            }
            for (thread_id, version) in reads {
                session.rpc_request_id += 1;
                let read_id = session.rpc_request_id;
                websocket
                    .send(Message::Text(
                        json!({"method":"thread/read","id":read_id,
                    "params":{"threadId":thread_id,"includeTurns":true}})
                        .to_string()
                        .into(),
                    ))
                    .await?;
                wait_for_response(
                    websocket,
                    writer,
                    session,
                    json!(read_id),
                    "thread/read/response",
                    Some(&thread_id),
                )
                .await?;
                session.reconciled_threads.insert(thread_id, version);
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
    }
    Ok(())
}

async fn attach_loaded_threads(
    websocket: &mut WebSocketStream<UnixStream>,
    writer: &WriterHandle,
    database: &Database,
    session: &mut LiveSession,
) -> Result<()> {
    let mut cursor: Option<String> = None;
    let mut request_id = 10_i64;
    loop {
        websocket
            .send(Message::Text(
                json!({"method":"thread/loaded/list","id":request_id,"params":{"cursor":cursor,"limit":200}})
                    .to_string()
                    .into(),
            ))
            .await?;
        let response = wait_for_response(
            websocket,
            writer,
            session,
            json!(request_id),
            "thread/loaded/list/response",
            None,
        )
        .await?;
        let result = response.get("result").cloned().unwrap_or(Value::Null);
        let thread_ids = result
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for thread_id in thread_ids.iter().filter_map(Value::as_str) {
            if session.attached_threads.contains(thread_id) {
                continue;
            }
            if database
                .active_thread_lease_owner(&session.app_source_id, &session.epoch_id, thread_id)?
                .is_some()
            {
                continue;
            }
            request_id += 1;
            websocket
                .send(Message::Text(
                    json!({"method":"thread/resume","id":request_id,"params":{"threadId":thread_id}})
                        .to_string()
                        .into(),
                ))
                .await?;
            let response = wait_for_response(
                websocket,
                writer,
                session,
                json!(request_id),
                "thread/resume/response",
                Some(thread_id),
            )
            .await;
            match response {
                Ok(_) => {
                    session.attached_threads.insert(thread_id.to_string());
                }
                Err(error) => {
                    tracing::warn!(source_id = %session.app_source_id, thread_id, error = %error, "attach loaded thread failed");
                }
            }
        }
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
        request_id += 1;
    }
    Ok(())
}

async fn detach_worker_owned_threads(
    websocket: &mut WebSocketStream<UnixStream>,
    writer: &WriterHandle,
    database: &Database,
    session: &mut LiveSession,
) -> Result<()> {
    let mut leased = Vec::new();
    for thread_id in &session.attached_threads {
        if database
            .active_thread_lease_owner(&session.app_source_id, &session.epoch_id, thread_id)?
            .is_some()
        {
            leased.push(thread_id.clone());
        }
    }
    for thread_id in leased {
        session.rpc_request_id += 1;
        let request_id = session.rpc_request_id;
        websocket
            .send(Message::Text(
                json!({
                    "method":"thread/unsubscribe",
                    "id":request_id,
                    "params":{"threadId":thread_id}
                })
                .to_string()
                .into(),
            ))
            .await?;
        match wait_for_response(
            websocket,
            writer,
            session,
            json!(request_id),
            "thread/unsubscribe/response",
            Some(&thread_id),
        )
        .await
        {
            Ok(_) => {
                session.attached_threads.remove(&thread_id);
            }
            Err(error) => {
                tracing::warn!(
                    source_id = %session.app_source_id,
                    thread_id,
                    error = %error,
                    "failed to detach Supervisor from Session-owned Thread"
                );
            }
        }
    }
    Ok(())
}

async fn discover_loaded_threads<S>(
    websocket: &mut WebSocketStream<S>,
    writer: &WriterHandle,
    session: &mut LiveSession,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut cursor: Option<String> = None;
    loop {
        session.rpc_request_id += 1;
        let request_id = session.rpc_request_id;
        websocket
            .send(Message::Text(
                json!({"method":"thread/loaded/list","id":request_id,"params":{"cursor":cursor,"limit":200}})
                    .to_string()
                    .into(),
            ))
            .await?;
        let response = wait_for_response(
            websocket,
            writer,
            session,
            json!(request_id),
            "thread/loaded/list/response",
            None,
        )
        .await?;
        let result = response.get("result").cloned().unwrap_or(Value::Null);
        let thread_ids = result
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect::<Vec<_>>();
        for thread_id in thread_ids {
            session.attached_threads.insert(thread_id.clone());
            session.rpc_request_id += 1;
            let read_id = session.rpc_request_id;
            websocket
                .send(Message::Text(
                    json!({"method":"thread/read","id":read_id,"params":{"threadId":thread_id,"includeTurns":true}})
                        .to_string()
                        .into(),
                ))
                .await?;
            let response = wait_for_correlated_response(
                websocket,
                writer,
                session,
                json!(read_id),
                "thread/read/response",
                Some(&thread_id),
            )
            .await?;
            if unmaterialized_thread_read(&response) {
                continue;
            }
            if let Some(error) = response.get("error") {
                if error.get("code").and_then(Value::as_i64) == Some(-32601) {
                    bail!(
                        "incompatible protocol: stable method thread/read/response is unavailable"
                    );
                }
                bail!("app-server request failed: {error}");
            }
            update_thread_runtime_from_result(session, &response);
            refresh_thread_goal(websocket, writer, session, &thread_id).await?;
        }
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(())
}

fn unmaterialized_thread_read(response: &Value) -> bool {
    response.pointer("/error/code").and_then(Value::as_i64) == Some(-32600)
        && response
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|message| {
                message.contains("is not materialized yet")
                    && message.contains("includeTurns is unavailable before first user message")
            })
}

async fn refresh_thread_goal<S>(
    websocket: &mut WebSocketStream<S>,
    writer: &WriterHandle,
    session: &mut LiveSession,
    thread_id: &str,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    session.rpc_request_id += 1;
    let request_id = session.rpc_request_id;
    websocket
        .send(Message::Text(
            json!({"method":"thread/goal/get","id":request_id,"params":{"threadId":thread_id}})
                .to_string()
                .into(),
        ))
        .await?;
    let response = wait_for_correlated_response(
        websocket,
        writer,
        session,
        json!(request_id),
        "thread/goal/get/response",
        Some(thread_id),
    )
    .await?;
    session
        .capability_catalog
        .record_availability("thread/goal/get", false, &response);
    Ok(())
}

fn update_thread_runtime_from_result(
    session: &mut LiveSession,
    envelope: &Value,
) -> Option<String> {
    let thread = envelope.pointer("/result/thread")?;
    let thread_id = thread.get("id")?.as_str()?.to_string();
    session.attached_threads.insert(thread_id.clone());
    let active_turn = thread
        .get("turns")
        .and_then(Value::as_array)
        .and_then(|turns| {
            turns
                .iter()
                .rev()
                .find(|turn| turn.get("status").and_then(Value::as_str) == Some("inProgress"))
        })
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(turn_id) = active_turn {
        session.active_turns.insert(thread_id.clone(), turn_id);
    } else {
        session.active_turns.remove(&thread_id);
    }
    if let Some(status) = thread
        .pointer("/status/type")
        .or_else(|| thread.get("status"))
        .and_then(Value::as_str)
    {
        session
            .thread_statuses
            .insert(thread_id.clone(), status.to_string());
    }
    if let Some(model) = envelope
        .pointer("/result/model")
        .or_else(|| thread.get("model"))
        .and_then(Value::as_str)
    {
        session
            .thread_models
            .insert(thread_id.clone(), model.to_string());
    }
    if let Some(effort) = envelope
        .pointer("/result/reasoningEffort")
        .or_else(|| envelope.pointer("/result/effort"))
        .cloned()
    {
        session
            .thread_reasoning_efforts
            .insert(thread_id.clone(), effort);
    }
    Some(thread_id)
}

#[derive(Debug)]
struct CommandWaitFailure {
    reason_code: &'static str,
    close_source: bool,
}

async fn wait_for_command_response<S>(
    websocket: &mut WebSocketStream<S>,
    writer: &WriterHandle,
    session: &mut LiveSession,
    expected_id: Value,
    response_method: &str,
    thread_hint: Option<&str>,
) -> std::result::Result<Value, CommandWaitFailure>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let message = match tokio::time::timeout(COMMAND_RESPONSE_TIMEOUT, websocket.next()).await {
            Err(_) => {
                return Err(CommandWaitFailure {
                    reason_code: "UPSTREAM_TIMEOUT",
                    close_source: false,
                });
            }
            Ok(None) => {
                return Err(CommandWaitFailure {
                    reason_code: "SOURCE_DISCONNECTED",
                    close_source: true,
                });
            }
            Ok(Some(Err(_))) => {
                return Err(CommandWaitFailure {
                    reason_code: "SOURCE_DISCONNECTED",
                    close_source: true,
                });
            }
            Ok(Some(Ok(message))) => message,
        };
        match message {
            Message::Text(text) => {
                let envelope: Value =
                    serde_json::from_str(&text).map_err(|_| CommandWaitFailure {
                        reason_code: "UPSTREAM_PROTOCOL_INVALID",
                        close_source: true,
                    })?;
                let matched = envelope.get("id") == Some(&expected_id)
                    && (envelope.get("result").is_some() || envelope.get("error").is_some());
                let observation = ingest_envelope(
                    writer,
                    session,
                    &envelope,
                    matched.then_some(response_method),
                    matched.then_some(thread_hint).flatten(),
                )
                .map_err(|_| CommandWaitFailure {
                    reason_code: "UPSTREAM_PROTOCOL_INVALID",
                    close_source: true,
                })?;
                apply_control_observation(writer, session, observation).map_err(|_| {
                    CommandWaitFailure {
                        reason_code: "COMMAND_RECONCILIATION_FAILED",
                        close_source: true,
                    }
                })?;
                if matched {
                    return Ok(envelope);
                }
            }
            Message::Ping(payload) => {
                websocket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|_| CommandWaitFailure {
                        reason_code: "SOURCE_DISCONNECTED",
                        close_source: true,
                    })?
            }
            Message::Close(_) => {
                return Err(CommandWaitFailure {
                    reason_code: "SOURCE_DISCONNECTED",
                    close_source: true,
                });
            }
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

async fn dispatch_actor_command<S>(
    websocket: &mut WebSocketStream<S>,
    writer: &WriterHandle,
    database: &Database,
    session: &mut LiveSession,
    command: ActorCommand,
) -> Result<(GatewayCommandRecord, bool)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if let Some((thread_id, supplied_thread_key)) = command.operation.thread_target() {
        let expected_thread_key = thread_key(&session.store_source_id, thread_id);
        if supplied_thread_key != expected_thread_key {
            let record = transition_gateway(
                writer,
                &command.command_id,
                "rejected",
                None,
                Some("THREAD_NOT_LOADED"),
                Some("the selected Thread does not belong to this source"),
                Some("THREAD_NOT_LOADED"),
                "deny",
                "rejected",
            )?;
            return Ok((record, false));
        }
        if database
            .active_thread_lease_owner(&session.app_source_id, &session.epoch_id, thread_id)?
            .is_some()
        {
            let record = transition_gateway(
                writer,
                &command.command_id,
                "rejected",
                None,
                Some("THREAD_OWNED_BY_SESSION"),
                Some("the selected Thread is owned by a Session Worker; attach to that Session"),
                Some("THREAD_OWNED_BY_SESSION"),
                "deny",
                "rejected",
            )?;
            return Ok((record, false));
        }
    }
    let unavailable_method = command
        .operation
        .method_and_params()
        .map(|(method, _)| method)
        .filter(|method| !session.method_available(method))
        .or_else(|| {
            matches!(&command.operation, ControllerOperation::Plan { .. })
                .then_some("thread/settings/update")
                .filter(|method| !session.method_available(method))
        });
    if unavailable_method.is_some() {
        let record = transition_gateway(
            writer,
            &command.command_id,
            "rejected",
            None,
            Some("CAPABILITY_UNAVAILABLE"),
            Some("the requested App Server method is unavailable for this source epoch"),
            Some("CAPABILITY_UNAVAILABLE"),
            "deny",
            "rejected",
        )?;
        return Ok((record, false));
    }
    let precondition_error = match &command.operation {
        ControllerOperation::TurnStart { thread_id, .. } => {
            if !session.attached_threads.contains(thread_id) {
                Some((
                    "THREAD_NOT_LOADED",
                    "the selected Thread is not loaded by this source",
                ))
            } else if session.active_turns.contains_key(thread_id) {
                Some((
                    "TURN_STATE_CONFLICT",
                    "the selected Thread already has an active Turn",
                ))
            } else {
                None
            }
        }
        ControllerOperation::TurnSteer {
            thread_id,
            expected_turn_id,
            ..
        }
        | ControllerOperation::TurnInterrupt {
            thread_id,
            expected_turn_id,
            ..
        } => {
            if !session.attached_threads.contains(thread_id) {
                Some((
                    "THREAD_NOT_LOADED",
                    "the selected Thread is not loaded by this source",
                ))
            } else if session.active_turns.get(thread_id) != Some(expected_turn_id) {
                Some((
                    "TURN_STATE_CONFLICT",
                    "expectedTurnId does not match the active Turn",
                ))
            } else {
                None
            }
        }
        ControllerOperation::ThreadStart {
            model, permissions, ..
        } => {
            if model
                .as_deref()
                .is_some_and(|id| !session.capability_catalog.selectable_id("model/list", id))
                || permissions.as_deref().is_some_and(|id| {
                    !session
                        .capability_catalog
                        .selectable_id("permissionProfile/list", id)
                })
            {
                Some((
                    "CAPABILITY_UNAVAILABLE",
                    "the selected model or permission profile is not available on this source",
                ))
            } else {
                None
            }
        }
        ControllerOperation::ThreadSettingsUpdate {
            thread_id, setting, ..
        } => {
            if !session.attached_threads.contains(thread_id) {
                Some((
                    "THREAD_NOT_LOADED",
                    "the selected Thread is not loaded by this source",
                ))
            } else {
                let available = match setting {
                    ThreadSetting::Model(model) => session
                        .capability_catalog
                        .selectable_id("model/list", model),
                    ThreadSetting::ReasoningEffort(effort) => {
                        session.thread_models.get(thread_id).is_some_and(|model| {
                            session
                                .capability_catalog
                                .model_supports_effort(model, effort)
                        })
                    }
                    ThreadSetting::Personality(_) => {
                        session.thread_models.get(thread_id).is_some_and(|model| {
                            session.capability_catalog.model_supports_personality(model)
                        })
                    }
                    ThreadSetting::Permissions(profile) => session
                        .capability_catalog
                        .selectable_id("permissionProfile/list", profile),
                };
                (!available).then_some((
                    "CAPABILITY_UNAVAILABLE",
                    "the selected setting is not available for this source and Thread",
                ))
            }
        }
        ControllerOperation::Plan {
            thread_id,
            mode,
            expected_turn_id,
            prompt,
            ..
        } => {
            if !session.attached_threads.contains(thread_id) {
                Some((
                    "THREAD_NOT_LOADED",
                    "the selected Thread is not loaded by this source",
                ))
            } else if session
                .capability_catalog
                .collaboration_mode(
                    mode,
                    session.thread_models.get(thread_id).map(String::as_str),
                    session.thread_reasoning_efforts.get(thread_id),
                )
                .is_none()
            {
                Some((
                    "CAPABILITY_UNAVAILABLE",
                    "the selected collaboration mode is not available for this source and Thread",
                ))
            } else if mode != "plan" && prompt.is_some() {
                Some((
                    "COMMAND_INVALID",
                    "only Plan collaboration mode accepts a prompt",
                ))
            } else if prompt.is_some()
                && session.active_turns.get(thread_id).map(String::as_str)
                    != expected_turn_id.as_deref()
            {
                Some((
                    "TURN_STATE_CONFLICT",
                    "expectedTurnId does not match the active Turn",
                ))
            } else {
                None
            }
        }
        ControllerOperation::ThreadCompact { thread_id, .. }
        | ControllerOperation::ReviewStart { thread_id, .. } => {
            if !session.attached_threads.contains(thread_id) {
                Some((
                    "THREAD_NOT_LOADED",
                    "the selected Thread is not loaded by this source",
                ))
            } else if session.active_turns.contains_key(thread_id) {
                Some((
                    "TURN_STATE_CONFLICT",
                    "the selected Thread already has an active Turn",
                ))
            } else {
                None
            }
        }
        ControllerOperation::GoalGet { thread_id, .. }
        | ControllerOperation::GoalSet { thread_id, .. }
        | ControllerOperation::GoalClear { thread_id, .. } => {
            if !session.attached_threads.contains(thread_id) {
                Some((
                    "THREAD_NOT_LOADED",
                    "the selected Thread is not loaded by this source",
                ))
            } else {
                session
                    .capability_catalog
                    .entries
                    .get("thread/goal/get")
                    .is_some_and(|entry| !entry.available)
                    .then_some((
                        "CAPABILITY_UNAVAILABLE",
                        "Goal control is unavailable for this source",
                    ))
            }
        }
        ControllerOperation::PendingRequestAction {
            thread_id,
            request_id,
            expected_request_version,
            action,
            ..
        } => match session.pending_requests.get(request_id) {
            None => Some((
                "REQUEST_ALREADY_RESOLVED",
                "the pending request is no longer available on this source",
            )),
            Some(request) if request.thread_id != *thread_id => Some((
                "REQUEST_NOT_PENDING",
                "the pending request does not belong to the selected Thread",
            )),
            Some(request) if request.version != *expected_request_version => Some((
                "REQUEST_NOT_PENDING",
                "the pending request version no longer matches",
            )),
            Some(request) => request.response_for(action).err(),
        },
        ControllerOperation::ThreadResume { .. }
        | ControllerOperation::ThreadFork { .. }
        | ControllerOperation::ThreadNameSet { .. }
        | ControllerOperation::ThreadArchive { .. } => None,
    };
    if let Some((code, message)) = precondition_error {
        let record = transition_gateway(
            writer,
            &command.command_id,
            "rejected",
            None,
            Some(code),
            Some(message),
            Some(code),
            "deny",
            "rejected",
        )?;
        return Ok((record, false));
    }

    if matches!(
        command.operation,
        ControllerOperation::PendingRequestAction { .. }
    ) {
        return dispatch_pending_request_action(websocket, writer, session, command).await;
    }

    transition_gateway(
        writer,
        &command.command_id,
        "dispatching",
        None,
        None,
        None,
        None,
        "allow",
        "dispatching",
    )?;
    let plan_mode = if let ControllerOperation::Plan {
        thread_id, mode, ..
    } = &command.operation
    {
        if mode == "default" {
            session
                .thread_default_collaboration_modes
                .get(thread_id)
                .cloned()
                .or_else(|| {
                    session.capability_catalog.collaboration_mode(
                        mode,
                        session.thread_models.get(thread_id).map(String::as_str),
                        session.thread_reasoning_efforts.get(thread_id),
                    )
                })
        } else {
            let default_mode = session.capability_catalog.collaboration_mode(
                "default",
                session.thread_models.get(thread_id).map(String::as_str),
                session.thread_reasoning_efforts.get(thread_id),
            );
            if let Some(default_mode) = default_mode {
                session
                    .thread_default_collaboration_modes
                    .entry(thread_id.clone())
                    .or_insert(default_mode);
            }
            session.capability_catalog.collaboration_mode(
                mode,
                session.thread_models.get(thread_id).map(String::as_str),
                session.thread_reasoning_efforts.get(thread_id),
            )
        }
    } else {
        None
    };
    let (method, params) = match &command.operation {
        ControllerOperation::Plan {
            thread_id,
            expected_turn_id,
            client_user_message_id,
            prompt: Some(prompt),
            ..
        } if expected_turn_id.is_none() => (
            "turn/start",
            json!({
                "threadId":thread_id,
                "input":[{"type":"text","text":prompt}],
                "clientUserMessageId":client_user_message_id,
                "collaborationMode":plan_mode.clone(),
            }),
        ),
        ControllerOperation::Plan { thread_id, .. } => (
            "thread/settings/update",
            json!({"threadId":thread_id,"collaborationMode":plan_mode.clone()}),
        ),
        operation => operation
            .method_and_params()
            .expect("non-Plan operation has one fixed protocol request"),
    };
    session.rpc_request_id += 1;
    let request_id = session.rpc_request_id;
    if let ControllerOperation::TurnStart { thread_id, .. }
    | ControllerOperation::ReviewStart { thread_id, .. }
    | ControllerOperation::Plan {
        thread_id,
        prompt: Some(_),
        expected_turn_id: None,
        ..
    } = &command.operation
    {
        session.dispatching_turn_command = Some((command.command_id.clone(), thread_id.clone()));
    }
    let send_result = websocket
        .send(Message::Text(
            json!({"method":method,"id":request_id,"params":params})
                .to_string()
                .into(),
        ))
        .await;
    if send_result.is_err() {
        session.dispatching_turn_command = None;
        let record = transition_gateway(
            writer,
            &command.command_id,
            "outcome_unknown",
            None,
            Some("OUTCOME_UNKNOWN"),
            Some("the source disconnected after mutation dispatch began"),
            Some("SOURCE_DISCONNECTED"),
            "allow",
            "outcome_unknown",
        )?;
        return Ok((record, true));
    }

    let thread_hint = command
        .operation
        .thread_target()
        .map(|(thread_id, _)| thread_id);
    let response_method = format!("{method}/response");
    let response = match wait_for_command_response(
        websocket,
        writer,
        session,
        json!(request_id),
        &response_method,
        thread_hint,
    )
    .await
    {
        Ok(response) => response,
        Err(failure) => {
            session.dispatching_turn_command = None;
            let record = transition_gateway(
                writer,
                &command.command_id,
                "outcome_unknown",
                None,
                Some("OUTCOME_UNKNOWN"),
                Some("the upstream mutation result could not be confirmed"),
                Some(failure.reason_code),
                "allow",
                "outcome_unknown",
            )?;
            return Ok((record, failure.close_source));
        }
    };
    if response.get("error").is_some() {
        if response.pointer("/error/code").and_then(Value::as_i64) == Some(-32601) {
            session.capability_catalog.record_availability(
                method,
                matches!(&command.operation, ControllerOperation::Plan { .. }),
                &response,
            );
        }
        session.dispatching_turn_command = None;
        let (error_code, error_message) = safe_upstream_rejection(&command.operation, &response);
        let record = transition_gateway(
            writer,
            &command.command_id,
            "rejected",
            None,
            Some(error_code),
            Some(error_message),
            Some(error_code),
            "allow",
            "rejected",
        )?;
        return Ok((record, false));
    }

    let accepted = transition_gateway(
        writer,
        &command.command_id,
        "accepted_by_source",
        None,
        None,
        None,
        None,
        "allow",
        "accepted_by_source",
    )?;
    let result = response.get("result").cloned().unwrap_or(Value::Null);
    let record = match &command.operation {
        ControllerOperation::ThreadStart { .. }
        | ControllerOperation::ThreadResume { .. }
        | ControllerOperation::ThreadFork { .. } => {
            let Some(thread_id) = update_thread_runtime_from_result(session, &response) else {
                session.dispatching_turn_command = None;
                return transition_gateway(
                    writer,
                    &command.command_id,
                    "outcome_unknown",
                    None,
                    Some("OUTCOME_UNKNOWN"),
                    Some("the App Server response did not identify the resulting Thread"),
                    Some("UPSTREAM_PROTOCOL_INVALID"),
                    "allow",
                    "outcome_unknown",
                )
                .map(|record| (record, false));
            };
            transition_gateway(
                writer,
                &command.command_id,
                "completed",
                Some(json!({"threadId":thread_id})),
                None,
                None,
                None,
                "allow",
                "completed",
            )?
        }
        ControllerOperation::TurnStart { thread_id, .. } => {
            let Some(turn_id) = result
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                session.dispatching_turn_command = None;
                return transition_gateway(
                    writer,
                    &command.command_id,
                    "outcome_unknown",
                    None,
                    Some("OUTCOME_UNKNOWN"),
                    Some("the App Server response did not identify the resulting Turn"),
                    Some("UPSTREAM_PROTOCOL_INVALID"),
                    "allow",
                    "outcome_unknown",
                )
                .map(|record| (record, false));
            };
            session
                .active_turns
                .insert(thread_id.clone(), turn_id.clone());
            session
                .turn_commands
                .insert(turn_id.clone(), command.command_id.clone());
            session.dispatching_turn_command = None;
            let reconciled =
                if session.thread_statuses.get(thread_id).map(String::as_str) == Some("idle") {
                    reconcile_idle_thread_turn(websocket, writer, session, thread_id).await?
                } else {
                    None
                };
            let response_status = result.pointer("/turn/status").and_then(Value::as_str);
            let terminal_status = response_status
                .filter(|status| *status != "inProgress")
                .map(str::to_string)
                .or_else(|| session.terminal_turns.get(&turn_id).cloned());
            if let Some(record) = reconciled {
                record
            } else if let Some(status) = terminal_status {
                session.active_turns.remove(thread_id);
                session.turn_commands.remove(&turn_id);
                session.terminal_turns.remove(&turn_id);
                transition_turn_terminal(writer, &command.command_id, &turn_id, &status)?
            } else {
                transition_gateway(
                    writer,
                    &command.command_id,
                    "running",
                    Some(json!({"turnId":turn_id})),
                    None,
                    None,
                    None,
                    "allow",
                    "running",
                )?
            }
        }
        ControllerOperation::TurnSteer {
            expected_turn_id, ..
        } => transition_gateway(
            writer,
            &command.command_id,
            "completed",
            Some(json!({
                "turnId":result.get("turnId").and_then(Value::as_str).unwrap_or(expected_turn_id),
            })),
            None,
            None,
            None,
            "allow",
            "completed",
        )?,
        ControllerOperation::TurnInterrupt {
            expected_turn_id, ..
        } => transition_gateway(
            writer,
            &command.command_id,
            "completed",
            Some(json!({"turnId":expected_turn_id,"interruptRequested":true})),
            None,
            None,
            None,
            "allow",
            "completed",
        )?,
        ControllerOperation::ThreadSettingsUpdate {
            thread_id, setting, ..
        } => {
            if let ThreadSetting::Model(model) = setting {
                session
                    .thread_models
                    .insert(thread_id.clone(), model.clone());
            }
            let (setting_name, setting_value) = match setting {
                ThreadSetting::Model(value) => ("model", value),
                ThreadSetting::ReasoningEffort(value) => ("reasoning", value),
                ThreadSetting::Personality(value) => ("personality", value),
                ThreadSetting::Permissions(value) => ("permissions", value),
            };
            transition_gateway(
                writer,
                &command.command_id,
                "completed",
                Some(json!({"setting":setting_name,"value":setting_value})),
                None,
                None,
                None,
                "allow",
                "completed",
            )?
        }
        ControllerOperation::Plan {
            thread_id,
            mode: requested_mode,
            expected_turn_id,
            prompt,
            ..
        } => {
            let collaboration_mode = plan_mode
                .as_ref()
                .expect("Plan precondition resolved a collaboration mode");
            session
                .thread_collaboration_modes
                .insert(thread_id.clone(), collaboration_mode.clone());
            match (prompt.as_ref(), expected_turn_id.as_ref()) {
                (None, _) => transition_gateway(
                    writer,
                    &command.command_id,
                    "completed",
                    Some(json!({"collaborationMode":requested_mode})),
                    None,
                    None,
                    None,
                    "allow",
                    "completed",
                )?,
                (Some(_), None) => {
                    let Some(turn_id) = result
                        .pointer("/turn/id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                    else {
                        session.dispatching_turn_command = None;
                        return transition_gateway(
                            writer,
                            &command.command_id,
                            "outcome_unknown",
                            None,
                            Some("OUTCOME_UNKNOWN"),
                            Some("the App Server response did not identify the Plan Turn"),
                            Some("UPSTREAM_PROTOCOL_INVALID"),
                            "allow",
                            "outcome_unknown",
                        )
                        .map(|record| (record, false));
                    };
                    session
                        .active_turns
                        .insert(thread_id.clone(), turn_id.clone());
                    session
                        .turn_commands
                        .insert(turn_id.clone(), command.command_id.clone());
                    session.dispatching_turn_command = None;
                    let reconciled = if session.thread_statuses.get(thread_id).map(String::as_str)
                        == Some("idle")
                    {
                        reconcile_idle_thread_turn(websocket, writer, session, thread_id).await?
                    } else {
                        None
                    };
                    let terminal_status = result
                        .pointer("/turn/status")
                        .and_then(Value::as_str)
                        .filter(|status| *status != "inProgress")
                        .map(str::to_string)
                        .or_else(|| session.terminal_turns.get(&turn_id).cloned());
                    if let Some(record) = reconciled {
                        record
                    } else if let Some(status) = terminal_status {
                        session.active_turns.remove(thread_id);
                        session.turn_commands.remove(&turn_id);
                        session.terminal_turns.remove(&turn_id);
                        transition_turn_terminal(writer, &command.command_id, &turn_id, &status)?
                    } else {
                        transition_gateway(
                            writer,
                            &command.command_id,
                            "running",
                            Some(json!({"turnId":turn_id,"collaborationMode":"plan"})),
                            None,
                            None,
                            None,
                            "allow",
                            "running",
                        )?
                    }
                }
                (Some(prompt), Some(expected_turn_id)) => {
                    session.rpc_request_id += 1;
                    let steer_request_id = session.rpc_request_id;
                    let client_user_message_id = match &command.operation {
                        ControllerOperation::Plan {
                            client_user_message_id: Some(value),
                            ..
                        } => value,
                        _ => unreachable!(),
                    };
                    if websocket
                        .send(Message::Text(
                            json!({
                                "method":"turn/steer",
                                "id":steer_request_id,
                                "params":{
                                    "threadId":thread_id,
                                    "expectedTurnId":expected_turn_id,
                                    "input":[{"type":"text","text":prompt}],
                                    "clientUserMessageId":client_user_message_id,
                                }
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .is_err()
                    {
                        let record = transition_gateway(
                            writer,
                            &command.command_id,
                            "outcome_unknown",
                            Some(json!({"collaborationMode":"plan","followUp":"unconfirmed"})),
                            Some("OUTCOME_UNKNOWN"),
                            Some("Plan mode was applied but the steering outcome is unknown"),
                            Some("SOURCE_DISCONNECTED"),
                            "allow",
                            "outcome_unknown",
                        )?;
                        return Ok((record, true));
                    }
                    let steer_response = match wait_for_command_response(
                        websocket,
                        writer,
                        session,
                        json!(steer_request_id),
                        "turn/steer/response",
                        Some(thread_id),
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(failure) => {
                            let record = transition_gateway(
                                writer,
                                &command.command_id,
                                "outcome_unknown",
                                Some(json!({"collaborationMode":"plan","followUp":"unconfirmed"})),
                                Some("OUTCOME_UNKNOWN"),
                                Some("Plan mode was applied but the steering outcome is unknown"),
                                Some(failure.reason_code),
                                "allow",
                                "outcome_unknown",
                            )?;
                            return Ok((record, failure.close_source));
                        }
                    };
                    if steer_response.get("error").is_some() {
                        transition_gateway(
                            writer,
                            &command.command_id,
                            "outcome_unknown",
                            Some(json!({"collaborationMode":"plan","followUp":"rejected"})),
                            Some("OUTCOME_UNKNOWN"),
                            Some("Plan mode was applied but the App Server rejected steering"),
                            Some("UPSTREAM_REJECTED"),
                            "allow",
                            "outcome_unknown",
                        )?
                    } else {
                        let turn_id = steer_response
                            .pointer("/result/turnId")
                            .and_then(Value::as_str)
                            .unwrap_or(expected_turn_id);
                        transition_gateway(
                            writer,
                            &command.command_id,
                            "completed",
                            Some(json!({
                                "collaborationMode":"plan",
                                "turnId":turn_id,
                                "steered":true,
                            })),
                            None,
                            None,
                            None,
                            "allow",
                            "completed",
                        )?
                    }
                }
            }
        }
        ControllerOperation::ThreadNameSet { name, .. } => transition_gateway(
            writer,
            &command.command_id,
            "completed",
            Some(json!({"nameBytes":name.len()})),
            None,
            None,
            None,
            "allow",
            "completed",
        )?,
        ControllerOperation::ThreadArchive { thread_id, .. } => {
            session.attached_threads.remove(thread_id);
            session.active_turns.remove(thread_id);
            session.thread_models.remove(thread_id);
            session.thread_collaboration_modes.remove(thread_id);
            session.thread_goals.remove(thread_id);
            transition_gateway(
                writer,
                &command.command_id,
                "completed",
                Some(json!({"archived":true})),
                None,
                None,
                None,
                "allow",
                "completed",
            )?
        }
        ControllerOperation::ThreadCompact { .. } => transition_gateway(
            writer,
            &command.command_id,
            "completed",
            Some(json!({"compactStarted":true})),
            None,
            None,
            None,
            "allow",
            "completed",
        )?,
        ControllerOperation::ReviewStart { thread_id, .. } => {
            let Some(turn_id) = result
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                session.dispatching_turn_command = None;
                return transition_gateway(
                    writer,
                    &command.command_id,
                    "outcome_unknown",
                    None,
                    Some("OUTCOME_UNKNOWN"),
                    Some("the App Server response did not identify the review Turn"),
                    Some("UPSTREAM_PROTOCOL_INVALID"),
                    "allow",
                    "outcome_unknown",
                )
                .map(|record| (record, false));
            };
            session
                .active_turns
                .insert(thread_id.clone(), turn_id.clone());
            session
                .turn_commands
                .insert(turn_id.clone(), command.command_id.clone());
            session.dispatching_turn_command = None;
            let reconciled =
                if session.thread_statuses.get(thread_id).map(String::as_str) == Some("idle") {
                    reconcile_idle_thread_turn(websocket, writer, session, thread_id).await?
                } else {
                    None
                };
            let terminal_status = result
                .pointer("/turn/status")
                .and_then(Value::as_str)
                .filter(|status| *status != "inProgress")
                .map(str::to_string)
                .or_else(|| session.terminal_turns.get(&turn_id).cloned());
            if let Some(record) = reconciled {
                record
            } else if let Some(status) = terminal_status {
                session.active_turns.remove(thread_id);
                session.turn_commands.remove(&turn_id);
                session.terminal_turns.remove(&turn_id);
                transition_turn_terminal(writer, &command.command_id, &turn_id, &status)?
            } else {
                transition_gateway(
                    writer,
                    &command.command_id,
                    "running",
                    Some(json!({"turnId":turn_id,"reviewStarted":true})),
                    None,
                    None,
                    None,
                    "allow",
                    "running",
                )?
            }
        }
        ControllerOperation::GoalGet { .. } | ControllerOperation::GoalSet { .. } => {
            let goal = result.get("goal").filter(|value| !value.is_null());
            transition_gateway(
                writer,
                &command.command_id,
                "completed",
                Some(json!({
                    "goalPresent":goal.is_some(),
                    "goalStatus":goal.and_then(|value| value.get("status")).and_then(Value::as_str),
                })),
                None,
                None,
                None,
                "allow",
                "completed",
            )?
        }
        ControllerOperation::GoalClear { .. } => transition_gateway(
            writer,
            &command.command_id,
            "completed",
            Some(json!({"cleared":result.get("cleared").and_then(Value::as_bool).unwrap_or(true)})),
            None,
            None,
            None,
            "allow",
            "completed",
        )?,
        ControllerOperation::PendingRequestAction { .. } => {
            unreachable!("pending request actions use their server-response dispatcher")
        }
    };
    let _ = accepted;
    Ok((record, false))
}

fn safe_upstream_rejection(
    operation: &ControllerOperation,
    response: &Value,
) -> (&'static str, &'static str) {
    let message = response.pointer("/error/message").and_then(Value::as_str);
    if matches!(operation, ControllerOperation::ThreadResume { .. }) {
        if message.is_some_and(|value| value.contains("already has an active writer")) {
            return (
                "THREAD_IN_USE",
                "the selected Thread is already open in another Codex client",
            );
        }
        if message.is_some_and(|value| value.contains(" is archived")) {
            return (
                "THREAD_ARCHIVED",
                "the selected Thread must be unarchived before it can be resumed",
            );
        }
    }
    (
        "UPSTREAM_REJECTED",
        "the App Server rejected the requested operation",
    )
}

async fn dispatch_pending_request_action<S>(
    websocket: &mut WebSocketStream<S>,
    writer: &WriterHandle,
    session: &mut LiveSession,
    command: ActorCommand,
) -> Result<(GatewayCommandRecord, bool)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let ControllerOperation::PendingRequestAction {
        request_id, action, ..
    } = &command.operation
    else {
        unreachable!("pending request dispatcher received another operation")
    };
    let request = session
        .pending_requests
        .get(request_id)
        .cloned()
        .expect("pending request preconditions were checked");
    let (response, request_type) = request
        .response_for(action)
        .expect("pending request action was validated before claim");

    match writer.claim_pending_request(&command.command_id)? {
        PendingRequestClaim::Claimed(_) => {}
        PendingRequestClaim::NotPending => {
            let record = transition_gateway(
                writer,
                &command.command_id,
                "rejected",
                None,
                Some("REQUEST_NOT_PENDING"),
                Some("the pending request version no longer matches"),
                Some("REQUEST_NOT_PENDING"),
                "deny",
                "rejected",
            )?;
            return Ok((record, false));
        }
        PendingRequestClaim::AlreadyResolved => {
            let record = transition_gateway(
                writer,
                &command.command_id,
                "rejected",
                None,
                Some("REQUEST_ALREADY_RESOLVED"),
                Some("another client already resolved this request"),
                Some("REQUEST_ALREADY_RESOLVED"),
                "deny",
                "rejected",
            )?;
            return Ok((record, false));
        }
        PendingRequestClaim::SourceEpochStale => {
            let record = transition_gateway(
                writer,
                &command.command_id,
                "rejected",
                None,
                Some("SOURCE_EPOCH_STALE"),
                Some("the request belongs to an inactive source epoch"),
                Some("SOURCE_EPOCH_STALE"),
                "deny",
                "rejected",
            )?;
            return Ok((record, false));
        }
    }

    if websocket
        .send(Message::Text(
            json!({"id":request.rpc_id,"result":response})
                .to_string()
                .into(),
        ))
        .await
        .is_err()
    {
        let record = transition_gateway(
            writer,
            &command.command_id,
            "outcome_unknown",
            Some(json!({"requestType":request_type})),
            Some("OUTCOME_UNKNOWN"),
            Some("the source disconnected after the request response was dispatched"),
            Some("SOURCE_DISCONNECTED"),
            "allow",
            "outcome_unknown",
        )?;
        return Ok((record, true));
    }

    transition_gateway(
        writer,
        &command.command_id,
        "accepted_by_source",
        Some(json!({"requestType":request_type})),
        None,
        None,
        None,
        "allow",
        "accepted_by_source",
    )?;

    match wait_for_request_resolution(websocket, writer, session, request_id).await {
        Ok(()) => {
            let record = transition_gateway(
                writer,
                &command.command_id,
                "completed",
                Some(json!({"requestType":request_type,"resolved":true})),
                None,
                None,
                None,
                "allow",
                "completed",
            )?;
            Ok((record, false))
        }
        Err(failure) => {
            let record = transition_gateway(
                writer,
                &command.command_id,
                "outcome_unknown",
                Some(json!({"requestType":request_type})),
                Some("OUTCOME_UNKNOWN"),
                Some("the request response was written but resolution could not be confirmed"),
                Some(failure.reason_code),
                "allow",
                "outcome_unknown",
            )?;
            Ok((record, failure.close_source))
        }
    }
}

async fn wait_for_request_resolution<S>(
    websocket: &mut WebSocketStream<S>,
    writer: &WriterHandle,
    session: &mut LiveSession,
    expected_request_id: &str,
) -> std::result::Result<(), CommandWaitFailure>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let message = match tokio::time::timeout(COMMAND_RESPONSE_TIMEOUT, websocket.next()).await {
            Err(_) => {
                return Err(CommandWaitFailure {
                    reason_code: "UPSTREAM_TIMEOUT",
                    close_source: false,
                });
            }
            Ok(None) | Ok(Some(Err(_))) => {
                return Err(CommandWaitFailure {
                    reason_code: "SOURCE_DISCONNECTED",
                    close_source: true,
                });
            }
            Ok(Some(Ok(message))) => message,
        };
        match message {
            Message::Text(text) => {
                let envelope: Value =
                    serde_json::from_str(&text).map_err(|_| CommandWaitFailure {
                        reason_code: "UPSTREAM_PROTOCOL_INVALID",
                        close_source: true,
                    })?;
                let resolved = envelope.get("method").and_then(Value::as_str)
                    == Some("serverRequest/resolved")
                    && id_string(envelope.pointer("/params/requestId")).as_deref()
                        == Some(expected_request_id);
                let observation =
                    ingest_envelope(writer, session, &envelope, None, None).map_err(|_| {
                        CommandWaitFailure {
                            reason_code: "UPSTREAM_PROTOCOL_INVALID",
                            close_source: true,
                        }
                    })?;
                apply_control_observation(writer, session, observation).map_err(|_| {
                    CommandWaitFailure {
                        reason_code: "COMMAND_RECONCILIATION_FAILED",
                        close_source: true,
                    }
                })?;
                if resolved {
                    return Ok(());
                }
            }
            Message::Ping(payload) => {
                websocket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|_| CommandWaitFailure {
                        reason_code: "SOURCE_DISCONNECTED",
                        close_source: true,
                    })?
            }
            Message::Close(_) => {
                return Err(CommandWaitFailure {
                    reason_code: "SOURCE_DISCONNECTED",
                    close_source: true,
                });
            }
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

async fn wait_for_response<S>(
    websocket: &mut WebSocketStream<S>,
    writer: &WriterHandle,
    session: &mut LiveSession,
    expected_id: Value,
    response_method: &str,
    thread_hint: Option<&str>,
) -> Result<Value>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let envelope = wait_for_correlated_response(
        websocket,
        writer,
        session,
        expected_id,
        response_method,
        thread_hint,
    )
    .await?;
    if let Some(error) = envelope.get("error") {
        if error.get("code").and_then(Value::as_i64) == Some(-32601) {
            bail!("incompatible protocol: stable method {response_method} is unavailable");
        }
        bail!("app-server request failed: {error}");
    }
    Ok(envelope)
}

async fn wait_for_correlated_response<S>(
    websocket: &mut WebSocketStream<S>,
    writer: &WriterHandle,
    session: &mut LiveSession,
    expected_id: Value,
    response_method: &str,
    thread_hint: Option<&str>,
) -> Result<Value>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), websocket.next())
            .await
            .context("app-server response timeout")?
            .context("app-server closed before response")??;
        match message {
            Message::Text(text) => {
                let envelope: Value = serde_json::from_str(&text)?;
                if envelope.get("id") == Some(&expected_id)
                    && (envelope.get("result").is_some() || envelope.get("error").is_some())
                {
                    let observation = ingest_envelope(
                        writer,
                        session,
                        &envelope,
                        Some(response_method),
                        thread_hint,
                    )?;
                    apply_control_observation(writer, session, observation)?;
                    return Ok(envelope);
                }
                let observation = ingest_envelope(writer, session, &envelope, None, None)?;
                apply_control_observation(writer, session, observation)?;
            }
            Message::Ping(payload) => websocket.send(Message::Pong(payload)).await?,
            Message::Close(_) => bail!("app-server closed before response"),
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

#[derive(Debug)]
struct ControlObservation {
    method: String,
    thread_id: Option<String>,
    turn_id: Option<String>,
    turn_status: Option<String>,
    thread_status: Option<String>,
    thread_model: Option<String>,
    thread_reasoning_effort: Option<Value>,
    collaboration_mode: Option<Value>,
    goal: Option<Value>,
    goal_cleared: bool,
    pending_request: Option<(String, PendingServerRequest)>,
    resolved_request_id: Option<String>,
}

fn ingest_envelope(
    writer: &impl LiveIngest,
    session: &mut LiveSession,
    envelope: &Value,
    method_hint: Option<&str>,
    thread_hint: Option<&str>,
) -> Result<ControlObservation> {
    validate_envelope(envelope)?;
    session.source_seq += 1;
    let event = normalize_envelope(session, envelope, method_hint, thread_hint)?;
    let pending_request =
        if event.phase == "request" && SERVER_REQUEST_METHODS.contains(&event.method.as_str()) {
            event.request_id.as_ref().map(|request_id| {
                let version = session
                    .pending_requests
                    .get(request_id)
                    .map(|request| request.version + 1)
                    .unwrap_or(1);
                (
                    request_id.clone(),
                    PendingServerRequest {
                        rpc_id: envelope.get("id").cloned().unwrap_or(Value::Null),
                        method: event.method.clone(),
                        thread_id: event.codex_thread_id.clone(),
                        version,
                        payload: event.payload.clone(),
                    },
                )
            })
        } else {
            None
        };
    let resolved_request_id = (event.phase == "resolved")
        .then(|| event.request_id.clone())
        .flatten();
    let observation = ControlObservation {
        method: event.method.clone(),
        thread_id: (!event.codex_thread_id.is_empty()).then(|| event.codex_thread_id.clone()),
        turn_id: event.turn_id.clone(),
        turn_status: event
            .payload
            .get("status")
            .and_then(|status| status.get("type").unwrap_or(status).as_str())
            .map(str::to_string),
        thread_status: (event.method == "thread/status/changed")
            .then(|| {
                event
                    .payload
                    .get("status")
                    .and_then(|status| status.get("type").unwrap_or(status).as_str())
                    .map(str::to_string)
            })
            .flatten(),
        thread_model: event
            .payload
            .pointer("/threadSettings/model")
            .and_then(Value::as_str)
            .map(str::to_string),
        thread_reasoning_effort: event.payload.pointer("/threadSettings/effort").cloned(),
        collaboration_mode: event
            .payload
            .pointer("/threadSettings/collaborationMode")
            .filter(|value| !value.is_null())
            .cloned(),
        goal: event
            .payload
            .get("goal")
            .filter(|value| !value.is_null())
            .cloned(),
        goal_cleared: matches!(
            event.method.as_str(),
            "thread/goal/cleared" | "thread/goal/clear/response"
        ) || (event.method == "thread/goal/get/response"
            && event.payload.get("goal").is_some_and(Value::is_null)),
        pending_request,
        resolved_request_id,
    };
    let current_turn_id = event.turn_id.clone();
    let events = [event];
    writer.ingest_live(OwnedIngestBatch {
        source_id: session.app_source_id.clone(),
        epoch_id: session.epoch_id.clone(),
        checkpoint_key: format!("live:{}:{}", session.app_source_id, session.epoch_id),
        file_identity: session.socket_path.to_string_lossy().to_string(),
        byte_offset: 0,
        ordinal: session.source_seq as u64,
        current_turn_id,
        clean_eof: false,
        events: events.to_vec(),
    })?;
    Ok(observation)
}

fn apply_control_observation(
    writer: &WriterHandle,
    session: &mut LiveSession,
    observation: ControlObservation,
) -> Result<()> {
    if let Some((request_id, request)) = observation.pending_request.as_ref() {
        session
            .pending_requests
            .insert(request_id.clone(), request.clone());
    }
    if let Some(request_id) = observation.resolved_request_id.as_deref() {
        session.pending_requests.remove(request_id);
    }
    if let Some(thread_id) = observation.thread_id.as_deref() {
        if let Some(status) = observation.thread_status.as_deref() {
            session
                .thread_statuses
                .insert(thread_id.to_string(), status.to_string());
        }
        if observation.goal_cleared {
            session.thread_goals.remove(thread_id);
        } else if let Some(goal) = observation.goal.as_ref() {
            session
                .thread_goals
                .insert(thread_id.to_string(), goal.clone());
        }
    }
    if observation.method == "thread/settings/updated"
        && let (Some(thread_id), Some(model)) = (
            observation.thread_id.as_deref(),
            observation.thread_model.as_deref(),
        )
    {
        session
            .thread_models
            .insert(thread_id.to_string(), model.to_string());
    }
    if observation.method == "thread/settings/updated"
        && let (Some(thread_id), Some(effort)) = (
            observation.thread_id.as_deref(),
            observation.thread_reasoning_effort.as_ref(),
        )
    {
        session
            .thread_reasoning_efforts
            .insert(thread_id.to_string(), effort.clone());
    }
    if observation.method == "thread/settings/updated"
        && let (Some(thread_id), Some(mode)) = (
            observation.thread_id.as_deref(),
            observation.collaboration_mode.as_ref(),
        )
    {
        session
            .thread_collaboration_modes
            .insert(thread_id.to_string(), mode.clone());
        if mode.get("mode").and_then(Value::as_str) != Some("plan") {
            session
                .thread_default_collaboration_modes
                .insert(thread_id.to_string(), mode.clone());
        }
    }
    if observation.method == "thread/started"
        && let Some(thread_id) = observation.thread_id.as_deref()
    {
        session.attached_threads.insert(thread_id.to_string());
    }
    if observation.method == "turn/started"
        && let (Some(thread_id), Some(turn_id)) = (
            observation.thread_id.as_deref(),
            observation.turn_id.as_deref(),
        )
    {
        session
            .active_turns
            .insert(thread_id.to_string(), turn_id.to_string());
        if let Some((command_id, expected_thread_id)) = session.dispatching_turn_command.as_ref()
            && expected_thread_id == thread_id
        {
            session
                .turn_commands
                .insert(turn_id.to_string(), command_id.clone());
        }
    }
    if observation.method == "turn/completed"
        && let Some(turn_id) = observation.turn_id.as_deref()
    {
        if let Some(thread_id) = observation.thread_id.as_deref()
            && session.active_turns.get(thread_id).map(String::as_str) == Some(turn_id)
        {
            session.active_turns.remove(thread_id);
        }
        let status = observation
            .turn_status
            .as_deref()
            .unwrap_or("completed")
            .to_string();
        session
            .terminal_turns
            .insert(turn_id.to_string(), status.clone());
        if let Some(command_id) = session.turn_commands.get(turn_id).cloned() {
            let still_dispatching = session
                .dispatching_turn_command
                .as_ref()
                .is_some_and(|(pending, _)| pending == &command_id);
            if !still_dispatching {
                transition_turn_terminal(writer, &command_id, turn_id, &status)?;
                session.turn_commands.remove(turn_id);
                session.terminal_turns.remove(turn_id);
            }
        }
    }
    Ok(())
}

async fn reconcile_idle_thread_turn<S>(
    websocket: &mut WebSocketStream<S>,
    writer: &WriterHandle,
    session: &mut LiveSession,
    thread_id: &str,
) -> Result<Option<GatewayCommandRecord>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(turn_id) = session.active_turns.get(thread_id).cloned() else {
        return Ok(None);
    };
    if session.thread_statuses.get(thread_id).map(String::as_str) != Some("idle") {
        return Ok(None);
    }

    session.rpc_request_id += 1;
    let request_id = session.rpc_request_id;
    websocket
        .send(Message::Text(
            json!({"method":"thread/read","id":request_id,
                "params":{"threadId":thread_id,"includeTurns":true}})
            .to_string()
            .into(),
        ))
        .await?;
    let response = wait_for_correlated_response(
        websocket,
        writer,
        session,
        json!(request_id),
        "thread/read/response",
        Some(thread_id),
    )
    .await?;
    if response.get("error").is_some() {
        tracing::warn!(thread_id, "thread/read could not reconcile an idle Turn");
        return Ok(None);
    }

    let terminal_status = response
        .pointer("/result/thread/turns")
        .and_then(Value::as_array)
        .and_then(|turns| {
            turns
                .iter()
                .find(|turn| turn.get("id").and_then(Value::as_str) == Some(turn_id.as_str()))
        })
        .and_then(|turn| turn.get("status"))
        .and_then(Value::as_str)
        .filter(|status| *status != "inProgress")
        .map(str::to_string);
    update_thread_runtime_from_result(session, &response);
    let Some(status) = terminal_status else {
        return Ok(None);
    };

    session.active_turns.remove(thread_id);
    session.terminal_turns.remove(&turn_id);
    let Some(command_id) = session.turn_commands.remove(&turn_id) else {
        return Ok(None);
    };
    transition_turn_terminal(writer, &command_id, &turn_id, &status).map(Some)
}

fn transition_turn_terminal(
    writer: &WriterHandle,
    command_id: &str,
    turn_id: &str,
    status: &str,
) -> Result<GatewayCommandRecord> {
    let (state, error_code, message, outcome) = match status {
        "completed" => ("completed", None, None, "completed"),
        "interrupted" => (
            "cancelled",
            Some("TURN_INTERRUPTED"),
            Some("the Codex Turn was interrupted"),
            "cancelled",
        ),
        "failed" => (
            "failed",
            Some("UPSTREAM_TURN_FAILED"),
            Some("the Codex Turn failed"),
            "failed",
        ),
        _ => (
            "failed",
            Some("UPSTREAM_TURN_FAILED"),
            Some("the Codex Turn ended with an unsupported status"),
            "failed",
        ),
    };
    let record = transition_gateway(
        writer,
        command_id,
        state,
        Some(json!({"turnId":turn_id,"turnStatus":status})),
        error_code,
        message,
        error_code,
        "allow",
        outcome,
    )?;
    for path in writer.cleanup_turn_images(turn_id)? {
        let _ = fs::remove_file(path);
    }
    Ok(record)
}

#[allow(clippy::too_many_arguments)]
fn transition_gateway(
    writer: &WriterHandle,
    command_id: &str,
    to_state: &str,
    result: Option<Value>,
    error_code: Option<&str>,
    error_message: Option<&str>,
    reason_code: Option<&str>,
    decision: &str,
    outcome: &str,
) -> Result<GatewayCommandRecord> {
    writer.transition_gateway_command(GatewayTransition {
        command_id: command_id.into(),
        to_state: to_state.into(),
        result_summary_json: result.map(|value| value.to_string()),
        error_code: error_code.map(str::to_string),
        error_message: error_message.map(str::to_string),
        reason_code: reason_code.map(str::to_string),
        decision: decision.into(),
        outcome: outcome.into(),
    })
}

fn mark_inflight_outcome_unknown(
    writer: &WriterHandle,
    session: &mut LiveSession,
    reason_code: &str,
) {
    let command_ids = session
        .turn_commands
        .values()
        .cloned()
        .collect::<BTreeSet<_>>();
    for command_id in command_ids {
        if let Err(error) = transition_gateway(
            writer,
            &command_id,
            "outcome_unknown",
            None,
            Some("OUTCOME_UNKNOWN"),
            Some("the source disconnected before the Turn outcome was confirmed"),
            Some(reason_code),
            "allow",
            "outcome_unknown",
        ) {
            tracing::error!(command_id, error = %error, "mark disconnected command outcome unknown failed");
        }
    }
    session.turn_commands.clear();
}

fn normalize_envelope(
    session: &LiveSession,
    envelope: &Value,
    method_hint: Option<&str>,
    thread_hint: Option<&str>,
) -> Result<NormalizedEvent> {
    let original = serde_json::to_vec(envelope)?;
    let source_fingerprint = blake3::keyed_hash(&session.fingerprint_key, &original)
        .to_hex()
        .to_string();
    let (redacted, mut redaction_audit) = redact::redact(envelope, &session.fingerprint_key);
    let method = redacted
        .get("method")
        .and_then(Value::as_str)
        .or(method_hint)
        .unwrap_or("app_server/unknown")
        .to_string();
    let params = redacted
        .get("params")
        .or_else(|| redacted.get("result"))
        .cloned()
        .unwrap_or(Value::Null);
    let thread_id = extract_string(
        &params,
        &[
            "/threadId",
            "/thread/id",
            "/turn/threadId",
            "/item/threadId",
        ],
    )
    .or_else(|| thread_hint.map(str::to_string));
    let turn_id = extract_string(&params, &["/turnId", "/turn/id", "/item/turnId"]);
    let item = params.get("item").cloned();
    let mut payload = item
        .clone()
        .or_else(|| params.get("turn").cloned())
        .or_else(|| params.get("thread").cloned())
        .unwrap_or_else(|| params.clone());
    let request = redacted.get("method").is_some() && redacted.get("id").is_some();
    let resolved = method == "serverRequest/resolved";
    let request_id = if resolved {
        id_string(params.pointer("/requestId"))
    } else {
        request.then(|| id_string(redacted.get("id"))).flatten()
    };
    let item_id =
        extract_string(&params, &["/itemId", "/item/id", "/callId"]).or_else(|| request_id.clone());
    let phase = if resolved {
        "resolved"
    } else if request {
        "request"
    } else if method.ends_with("/started") || method.ends_with("/start") {
        "started"
    } else if method.ends_with("/completed") || method.ends_with("/complete") {
        "completed"
    } else if method.ends_with("/delta") || method.contains("/updated") {
        "delta"
    } else {
        "snapshot"
    }
    .to_string();
    let item_type = classify_live_item(&method, item.as_ref());
    let mut summary = live_summary(&params, item.as_ref());
    let reasoning = item_type.as_deref() == Some("reasoning")
        || method.to_ascii_lowercase().contains("reasoning");
    let policy_marker = if reasoning && !session.keep_reasoning {
        payload = json!({"policy":"omitted","kind":"reasoning","retainedIdentity":true});
        summary = Some("[reasoning omitted by capture policy]".into());
        Some(payload.clone())
    } else if !session.keep_raw_json {
        Some(json!({"policy":"omitted","kind":"raw_json","retainedIdentity":true}))
    } else {
        None
    };
    if let Some(marker) = policy_marker.as_ref() {
        redaction_audit["capturePolicy"] = marker.clone();
    }
    let stored = serde_json::to_string(policy_marker.as_ref().unwrap_or(&redacted))?;
    let projectable = thread_id.is_some();
    let codex_thread_id = thread_id.unwrap_or_default();
    let observer_thread_key = if projectable {
        thread_key(&session.store_source_id, &codex_thread_id)
    } else {
        String::new()
    };
    Ok(NormalizedEvent {
        event_id: Uuid::now_v7().to_string(),
        source_id: session.app_source_id.clone(),
        store_source_id: session.store_source_id.clone(),
        epoch_id: session.epoch_id.clone(),
        source_seq: session.source_seq,
        dedupe_key: format!(
            "app:{}:{}:{}",
            session.app_source_id, session.epoch_id, session.source_seq
        ),
        observed_at_ms: now_ms(),
        event_at_ms: None,
        thread_key: observer_thread_key,
        codex_thread_id,
        turn_id,
        item_id,
        request_id,
        blob_id: None,
        protocol_direction: None,
        worker_id: None,
        worker_connection_epoch: None,
        proxy_seq: None,
        method,
        phase: phase.clone(),
        durability: "transient".into(),
        projectable,
        source_fingerprint,
        stored_raw_hash: blake3::hash(stored.as_bytes()).to_hex().to_string(),
        raw_json: stored,
        redaction_json: redaction_audit.to_string(),
        decode_status: "decoded".into(),
        decode_error: None,
        top_type: "app_server".into(),
        item_type,
        item_status: Some(
            match phase.as_str() {
                "started" => "started",
                "delta" => "streaming",
                "request" => "started",
                _ => "completed",
            }
            .into(),
        ),
        summary_text: summary,
        payload,
    })
}

fn extract_string(value: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn id_string(value: Option<&Value>) -> Option<String> {
    value.map(|value| {
        value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string())
    })
}

fn canonical_existing(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).with_context(|| format!("canonicalize {}", path.display()))
}

async fn unsubscribe_attached(
    websocket: &mut WebSocketStream<UnixStream>,
    session: &mut LiveSession,
) -> Result<()> {
    for (index, thread_id) in session.attached_threads.iter().enumerate() {
        websocket
            .send(Message::Text(
                json!({"method":"thread/unsubscribe","id":900000 + index,"params":{"threadId":thread_id}})
                    .to_string()
                    .into(),
            ))
            .await?;
    }
    tracing::info!(
        source_id = %session.app_source_id,
        threads = session.attached_threads.len(),
        "sent live unsubscribe requests"
    );
    websocket.send(Message::Close(None)).await?;
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(message) = websocket.next().await {
            if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                break;
            }
        }
    })
    .await;
    Ok(())
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

#[cfg(unix)]
fn validate_socket(path: &Path) -> Result<()> {
    validate_private_unix_socket(path, "configured app-server endpoint")
}

#[cfg(not(unix))]
fn validate_socket(path: &Path) -> Result<()> {
    validate_private_unix_socket(path, "configured app-server endpoint")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::gateway::{GatewayCommandTarget, NewGatewayCommand};
    use crate::domain::session::{RegisterSessionWorker, SessionWorkerRegistration};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::io::duplex;
    use tokio_tungstenite::tungstenite::protocol::Role;

    fn session() -> LiveSession {
        LiveSession {
            app_source_id: "app-source".into(),
            store_source_id: "store-source".into(),
            epoch_id: "epoch".into(),
            socket_path: PathBuf::from("/tmp/socket"),
            source_seq: 1,
            fingerprint_key: [1_u8; 32],
            attached_threads: BTreeSet::new(),
            active_turns: BTreeMap::new(),
            thread_statuses: BTreeMap::new(),
            thread_models: BTreeMap::new(),
            thread_reasoning_efforts: BTreeMap::new(),
            thread_collaboration_modes: BTreeMap::new(),
            thread_default_collaboration_modes: BTreeMap::new(),
            thread_goals: BTreeMap::new(),
            pending_requests: BTreeMap::new(),
            turn_commands: BTreeMap::new(),
            terminal_turns: BTreeMap::new(),
            dispatching_turn_command: None,
            reconciled_threads: BTreeMap::new(),
            rpc_request_id: 100,
            controller_enabled: false,
            capability_catalog: CapabilityCatalog::default(),
            keep_reasoning: true,
            keep_raw_json: true,
        }
    }

    fn gateway_writer() -> Result<(TempDir, Arc<Database>, WriterHandle)> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        database.upsert_source_kind(
            "app-source",
            "app_server",
            "/tmp/socket",
            &json!({}),
            "ready",
        )?;
        database.upsert_source("store-source", "/tmp/store", &json!({}), "ready")?;
        let writer = WriterHandle::start(database.clone(), 4096, 32, 512)?;
        Ok((temp, database, writer))
    }

    fn authorize_test_command(
        writer: &WriterHandle,
        command_id: &str,
        capability: &str,
        thread_id: Option<&str>,
    ) -> Result<()> {
        let target = GatewayCommandTarget {
            source_id: "app-source".into(),
            source_epoch: "epoch".into(),
            thread_key: thread_id.map(|id| thread_key("store-source", id)),
            codex_thread_id: thread_id.map(str::to_string),
            expected_turn_id: None,
            expected_request_id: None,
            expected_request_version: None,
        };
        writer.receive_gateway_command(NewGatewayCommand {
            command_id: command_id.into(),
            principal_id: "local_bearer".into(),
            capability: capability.into(),
            idempotency_key: format!("key-{command_id}"),
            payload_hash: format!("hash-{command_id}"),
            target,
            input_summary_json: json!({"textBytes":5}).to_string(),
            origin: GatewayCommandOrigin::LegacyApi,
        })?;
        transition_gateway(
            writer,
            command_id,
            "authorized",
            None,
            None,
            None,
            None,
            "allow",
            "authorized",
        )?;
        Ok(())
    }

    fn authorize_pending_request_command(
        writer: &WriterHandle,
        command_id: &str,
        request_id: &str,
        request_version: i64,
    ) -> Result<()> {
        writer.receive_gateway_command(NewGatewayCommand {
            command_id: command_id.into(),
            principal_id: "local_bearer".into(),
            capability: "request.action".into(),
            idempotency_key: format!("key-{command_id}"),
            payload_hash: format!("hash-{command_id}"),
            target: GatewayCommandTarget {
                source_id: "app-source".into(),
                source_epoch: "epoch".into(),
                thread_key: Some(thread_key("store-source", "thread")),
                codex_thread_id: Some("thread".into()),
                expected_turn_id: None,
                expected_request_id: Some(request_id.into()),
                expected_request_version: Some(request_version),
            },
            input_summary_json: json!({"jsonBytes":84}).to_string(),
            origin: GatewayCommandOrigin::LegacyApi,
        })?;
        transition_gateway(
            writer,
            command_id,
            "authorized",
            None,
            None,
            None,
            None,
            "allow",
            "authorized",
        )?;
        Ok(())
    }

    fn insert_pending_request(
        database: &Database,
        request_id: &str,
        request_type: &str,
        version: i64,
    ) -> Result<()> {
        database.connect()?.execute(
            "INSERT INTO pending_requests(source_id,epoch_id,request_id,thread_key,request_type,
               state,request_event_seq,payload_json,request_version)
             VALUES (?1,?2,?3,?4,?5,'pending',1,'{}',?6)",
            rusqlite::params![
                "app-source",
                "epoch",
                request_id,
                thread_key("store-source", "thread"),
                request_type,
                version
            ],
        )?;
        Ok(())
    }

    #[test]
    fn normalizes_turn_and_item_notifications() -> Result<()> {
        let turn = normalize_envelope(
            &session(),
            &json!({"method":"turn/started","params":{"threadId":"thread","turn":{"id":"turn"}}}),
            None,
            None,
        )?;
        assert_eq!(turn.turn_id.as_deref(), Some("turn"));
        assert_eq!(turn.durability, "transient");
        assert!(turn.projectable);

        let item = normalize_envelope(
            &session(),
            &json!({"method":"item/completed","params":{"threadId":"thread","turnId":"turn","item":{"id":"item","type":"agentMessage","content":[{"text":"done"}]}}}),
            None,
            None,
        )?;
        assert_eq!(item.item_type.as_deref(), Some("agent_message"));
        assert_eq!(item.summary_text.as_deref(), Some("done"));
        Ok(())
    }

    #[test]
    fn pending_request_actions_are_closed_and_schema_validated() {
        let command = PendingServerRequest {
            rpc_id: json!(7),
            method: "item/commandExecution/requestApproval".into(),
            thread_id: "thread".into(),
            version: 1,
            payload: json!({"availableDecisions":["accept","decline"]}),
        };
        assert!(
            command
                .response_for(&PendingRequestAction::Approval {
                    decision: "accept".into()
                })
                .is_ok()
        );
        assert_eq!(
            command
                .response_for(&PendingRequestAction::Approval {
                    decision: "acceptForSession".into()
                })
                .unwrap_err()
                .0,
            "CAPABILITY_UNAVAILABLE"
        );
        assert_eq!(
            command
                .response_for(&PendingRequestAction::UserInput {
                    answers: BTreeMap::new()
                })
                .unwrap_err()
                .0,
            "COMMAND_INVALID"
        );

        let elicitation = PendingServerRequest {
            rpc_id: json!("mcp-1"),
            method: "mcpServer/elicitation/request".into(),
            thread_id: "thread".into(),
            version: 1,
            payload: json!({
                "requestedSchema":{
                    "type":"object",
                    "properties":{"name":{"type":"string"},"count":{"type":"number"}},
                    "required":["name"]
                }
            }),
        };
        assert!(
            elicitation
                .response_for(&PendingRequestAction::McpElicitation {
                    action: "accept".into(),
                    content: Some(json!({"name":"safe","count":2}))
                })
                .is_ok()
        );
        for content in [
            json!({"count":2}),
            json!({"name":3}),
            json!({"name":"x","extra":1}),
        ] {
            assert_eq!(
                elicitation
                    .response_for(&PendingRequestAction::McpElicitation {
                        action: "accept".into(),
                        content: Some(content)
                    })
                    .unwrap_err()
                    .0,
                "COMMAND_INVALID"
            );
        }
    }

    #[tokio::test]
    async fn pending_request_response_preserves_rpc_id_and_completes_after_resolved_ingest()
    -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        insert_pending_request(&database, "7", "approval", 1)?;
        authorize_pending_request_command(&writer, "request-command", "7", 1)?;
        let mut live = session();
        live.attached_threads.insert("thread".into());
        live.pending_requests.insert(
            "7".into(),
            PendingServerRequest {
                rpc_id: json!(7),
                method: "item/commandExecution/requestApproval".into(),
                thread_id: "thread".into(),
                version: 1,
                payload: json!({"availableDecisions":["accept","decline"]}),
            },
        );
        let (client_io, server_io) = duplex(8192);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let upstream = tokio::spawn(async move {
            let Message::Text(response) = server.next().await.unwrap().unwrap() else {
                panic!("expected a JSON-RPC text response")
            };
            let response: Value = serde_json::from_str(&response).unwrap();
            assert_eq!(response["id"], json!(7));
            assert_eq!(response["result"], json!({"decision":"accept"}));
            server
                .send(Message::Text(
                    json!({"method":"serverRequest/resolved","params":{"threadId":"thread","requestId":7}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut live,
            ActorCommand {
                command_id: "request-command".into(),
                operation: ControllerOperation::PendingRequestAction {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    request_id: "7".into(),
                    expected_request_version: 1,
                    action: PendingRequestAction::Approval {
                        decision: "accept".into(),
                    },
                },
            },
        )
        .await?;
        upstream.await?;
        assert!(!close);
        assert_eq!(record.state, "completed");
        assert!(!live.pending_requests.contains_key("7"));
        let connection = database.connect()?;
        let (request_state, resolved_events): (String, i64) = connection.query_row(
            "SELECT p.state,(SELECT COUNT(*) FROM raw_events r
               WHERE r.method='serverRequest/resolved' AND r.request_id='7')
             FROM pending_requests p WHERE p.source_id='app-source' AND p.epoch_id='epoch'
               AND p.request_id='7'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(request_state, "resolved");
        assert_eq!(resolved_events, 1);
        Ok(())
    }

    #[tokio::test]
    async fn pending_request_timeout_after_write_is_outcome_unknown() -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        insert_pending_request(&database, "request", "approval", 1)?;
        authorize_pending_request_command(&writer, "timeout-command", "request", 1)?;
        let mut live = session();
        live.attached_threads.insert("thread".into());
        live.pending_requests.insert(
            "request".into(),
            PendingServerRequest {
                rpc_id: json!("request"),
                method: "item/fileChange/requestApproval".into(),
                thread_id: "thread".into(),
                version: 1,
                payload: json!({}),
            },
        );
        let (client_io, server_io) = duplex(4096);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let upstream = tokio::spawn(async move {
            let _ = server.next().await;
            tokio::time::sleep(Duration::from_millis(150)).await;
        });
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut live,
            ActorCommand {
                command_id: "timeout-command".into(),
                operation: ControllerOperation::PendingRequestAction {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    request_id: "request".into(),
                    expected_request_version: 1,
                    action: PendingRequestAction::Approval {
                        decision: "decline".into(),
                    },
                },
            },
        )
        .await?;
        upstream.await?;
        assert!(!close);
        assert_eq!(record.state, "outcome_unknown");
        assert_eq!(record.error.as_ref().unwrap().code, "OUTCOME_UNKNOWN");
        Ok(())
    }

    #[tokio::test]
    async fn rpc_correlation_ingests_interleaved_envelopes_before_matching_response() -> Result<()>
    {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        database.upsert_source_kind(
            "app-source",
            "app_server",
            "/tmp/socket",
            &json!({}),
            "ready",
        )?;
        database.upsert_source("store-source", "/tmp/store", &json!({}), "ready")?;
        let writer = WriterHandle::start(database.clone(), 4096, 16, 512)?;
        let (client_io, server_io) = duplex(16 * 1024);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let server = tokio::spawn(async move {
            server
                .send(Message::Text(
                    json!({"method":"thread/started","params":{"thread":{"id":"thread"}}})
                        .to_string()
                        .into(),
                ))
                .await?;
            server
                .send(Message::Text(
                    json!({"id":999,"result":{"data":[]}}).to_string().into(),
                ))
                .await?;
            server
                .send(Message::Text(
                    json!({"id":42,"result":{"data":[]}}).to_string().into(),
                ))
                .await?;
            Result::<()>::Ok(())
        });

        let mut session = session();
        session.source_seq = 0;
        let response = wait_for_correlated_response(
            &mut client,
            &writer,
            &mut session,
            json!(42),
            "model/list/response",
            None,
        )
        .await?;
        assert_eq!(response["id"], 42);
        server.await??;
        let count: i64 = database.connect()?.query_row(
            "SELECT COUNT(*) FROM raw_events WHERE source_id='app-source' AND epoch_id='epoch'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 3);
        Ok(())
    }

    #[tokio::test]
    async fn turn_start_dispatch_tracks_interleaved_notifications_and_terminal_outcome()
    -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        authorize_test_command(&writer, "command", "turn.start", Some("thread"))?;
        let (client_io, server_io) = duplex(32 * 1024);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let server = tokio::spawn(async move {
            let request = server.next().await.context("missing request")??;
            let Message::Text(request) = request else {
                bail!("expected text request");
            };
            let request: Value = serde_json::from_str(&request)?;
            assert_eq!(request["method"], "turn/start");
            assert_eq!(request["params"]["clientUserMessageId"], "client-message");
            assert_eq!(request["params"]["input"][0]["text"], "hello");
            let id = request["id"].clone();
            server
                .send(Message::Text(
                    json!({"method":"turn/started","params":{"threadId":"thread","turn":{"id":"turn","status":"inProgress","items":[]}}})
                        .to_string()
                        .into(),
                ))
                .await?;
            server
                .send(Message::Text(
                    json!({"id":id,"result":{"turn":{"id":"turn","status":"inProgress","items":[]}}})
                        .to_string()
                        .into(),
                ))
                .await?;
            server
                .send(Message::Text(
                    json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed","items":[]}}})
                        .to_string()
                        .into(),
                ))
                .await?;
            Result::<()>::Ok(())
        });

        let mut session = session();
        session.attached_threads.insert("thread".into());
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut session,
            ActorCommand {
                command_id: "command".into(),
                operation: ControllerOperation::TurnStart {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    client_user_message_id: "client-message".into(),
                    text: "hello".into(),
                    image_paths: Vec::new(),
                },
            },
        )
        .await?;
        assert_eq!(record.state, "running");
        assert!(!close);
        let message = client.next().await.context("missing terminal event")??;
        let Message::Text(message) = message else {
            bail!("expected terminal text event");
        };
        let envelope: Value = serde_json::from_str(&message)?;
        let observation = ingest_envelope(&writer, &mut session, &envelope, None, None)?;
        apply_control_observation(&writer, &mut session, observation)?;
        server.await??;

        let command = database
            .gateway_command("command")?
            .context("missing command")?;
        assert_eq!(command.state, "completed");
        let transitions: String = database.connect()?.query_row(
            "SELECT group_concat(to_state, ',') FROM command_transitions WHERE command_id='command' ORDER BY transition_seq",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            transitions,
            "received,authorized,dispatching,accepted_by_source,running,completed"
        );
        let raw_count: i64 = database.connect()?.query_row(
            "SELECT COUNT(*) FROM raw_events WHERE source_id='app-source'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(raw_count, 3);
        Ok(())
    }

    #[tokio::test]
    async fn idle_thread_status_reconciles_terminal_turn_when_notifications_are_missing()
    -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        authorize_test_command(&writer, "command", "turn.start", Some("thread"))?;
        let (client_io, server_io) = duplex(32 * 1024);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let server = tokio::spawn(async move {
            let request = server.next().await.context("missing turn request")??;
            let Message::Text(request) = request else {
                bail!("expected text request");
            };
            let request: Value = serde_json::from_str(&request)?;
            let id = request["id"].clone();
            server
                .send(Message::Text(
                    json!({"id":id,"result":{"turn":{"id":"turn","status":"inProgress","items":[]}}})
                        .to_string()
                        .into(),
                ))
                .await?;
            for status in ["active", "idle"] {
                server
                    .send(Message::Text(
                        json!({"method":"thread/status/changed","params":{
                            "threadId":"thread","status":{"type":status}}})
                        .to_string()
                        .into(),
                    ))
                    .await?;
            }
            let request = server
                .next()
                .await
                .context("missing reconciliation read")??;
            let Message::Text(request) = request else {
                bail!("expected thread/read request");
            };
            let request: Value = serde_json::from_str(&request)?;
            assert_eq!(request["method"], "thread/read");
            assert_eq!(request["params"]["threadId"], "thread");
            assert_eq!(request["params"]["includeTurns"], true);
            server
                .send(Message::Text(
                    json!({"id":request["id"],"result":{"thread":{
                        "id":"thread","status":{"type":"idle"},
                        "turns":[{"id":"turn","status":"completed","items":[]}]}}})
                    .to_string()
                    .into(),
                ))
                .await?;
            Result::<()>::Ok(())
        });

        let mut session = session();
        session.attached_threads.insert("thread".into());
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut session,
            ActorCommand {
                command_id: "command".into(),
                operation: ControllerOperation::TurnStart {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    client_user_message_id: "client-message".into(),
                    text: "hello".into(),
                    image_paths: Vec::new(),
                },
            },
        )
        .await?;
        assert_eq!(record.state, "running");
        assert!(!close);

        for expected_status in ["active", "idle"] {
            let message = client.next().await.context("missing status event")??;
            let Message::Text(message) = message else {
                bail!("expected status text event");
            };
            let envelope: Value = serde_json::from_str(&message)?;
            let observation = ingest_envelope(&writer, &mut session, &envelope, None, None)?;
            assert_eq!(observation.thread_status.as_deref(), Some(expected_status));
            apply_control_observation(&writer, &mut session, observation)?;
        }
        let reconciled =
            reconcile_idle_thread_turn(&mut client, &writer, &mut session, "thread").await?;
        server.await??;

        assert_eq!(
            reconciled.as_ref().map(|record| record.state.as_str()),
            Some("completed")
        );
        assert!(!session.active_turns.contains_key("thread"));
        assert!(!session.turn_commands.contains_key("turn"));
        assert_eq!(
            database
                .gateway_command("command")?
                .context("missing command")?
                .state,
            "completed"
        );
        Ok(())
    }

    #[tokio::test]
    async fn turn_start_rejects_unloaded_thread_without_writing_upstream() -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        authorize_test_command(&writer, "unloaded", "turn.start", Some("thread"))?;
        let (client_io, server_io) = duplex(4096);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let _server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut session(),
            ActorCommand {
                command_id: "unloaded".into(),
                operation: ControllerOperation::TurnStart {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    client_user_message_id: "client-message".into(),
                    text: "hello".into(),
                    image_paths: Vec::new(),
                },
            },
        )
        .await?;
        assert_eq!(record.state, "rejected");
        assert_eq!(
            record.error.as_ref().map(|error| error.code.as_str()),
            Some("THREAD_NOT_LOADED")
        );
        assert!(!close);
        assert_eq!(
            database
                .gateway_command("unloaded")?
                .context("missing command")?
                .state,
            "rejected"
        );
        Ok(())
    }

    #[tokio::test]
    async fn supervisor_rejects_mutation_for_session_owned_thread_without_writing_upstream()
    -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        authorize_test_command(
            &writer,
            "create-session-owner",
            "session.create",
            Some("thread"),
        )?;
        assert!(matches!(
            writer.register_session_worker(SessionWorkerRegistration {
                worker_id: "worker-owner".into(),
                create_command_id: "create-session-owner".into(),
                principal_id: "local_bearer".into(),
                source_id: "app-source".into(),
                source_epoch: "epoch".into(),
                mode: "resume".into(),
                canonical_cwd: "/synthetic".into(),
                rows: 24,
                cols: 80,
                runtime_dir_name: "worker-owner".into(),
                primary_lease_id: "lease-owner".into(),
                codex_thread_id: Some("thread".into()),
                reservation_id: None,
            })?,
            RegisterSessionWorker::Created { .. }
        ));
        authorize_test_command(&writer, "blocked", "turn.start", Some("thread"))?;
        let (client_io, server_io) = duplex(4096);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let mut live = session();
        live.attached_threads.insert("thread".into());
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut live,
            ActorCommand {
                command_id: "blocked".into(),
                operation: ControllerOperation::TurnStart {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    client_user_message_id: "client-message".into(),
                    text: "must not be written".into(),
                    image_paths: Vec::new(),
                },
            },
        )
        .await?;
        assert!(!close);
        assert_eq!(record.state, "rejected");
        assert_eq!(
            record.error.as_ref().map(|error| error.code.as_str()),
            Some("THREAD_OWNED_BY_SESSION")
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), server.next())
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn upstream_rejection_is_sanitized_and_disconnect_after_write_is_unknown() -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        authorize_test_command(&writer, "rejected", "turn.start", Some("thread"))?;
        let (client_io, server_io) = duplex(8192);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let server_task = tokio::spawn(async move {
            let request = server.next().await.context("missing request")??;
            let Message::Text(request) = request else {
                bail!("expected text request");
            };
            let request: Value = serde_json::from_str(&request)?;
            server
                .send(Message::Text(
                    json!({"id":request["id"],"error":{"code":-32000,"message":"private upstream payload"}})
                        .to_string()
                        .into(),
                ))
                .await?;
            Result::<()>::Ok(())
        });
        let mut live = session();
        live.attached_threads.insert("thread".into());
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut live,
            ActorCommand {
                command_id: "rejected".into(),
                operation: ControllerOperation::TurnStart {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    client_user_message_id: "message".into(),
                    text: "hello".into(),
                    image_paths: Vec::new(),
                },
            },
        )
        .await?;
        server_task.await??;
        assert_eq!(record.state, "rejected");
        assert!(!close);
        assert_eq!(
            record.error.as_ref().map(|error| error.code.as_str()),
            Some("UPSTREAM_REJECTED")
        );
        assert!(!serde_json::to_string(&record)?.contains("private upstream payload"));

        authorize_test_command(&writer, "unknown", "turn.start", Some("thread"))?;
        let (client_io, server_io) = duplex(8192);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let server_task = tokio::spawn(async move {
            let _ = server.next().await;
            server.close(None).await?;
            Result::<()>::Ok(())
        });
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut live,
            ActorCommand {
                command_id: "unknown".into(),
                operation: ControllerOperation::TurnStart {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    client_user_message_id: "message-2".into(),
                    text: "hello".into(),
                    image_paths: Vec::new(),
                },
            },
        )
        .await?;
        server_task.await??;
        assert_eq!(record.state, "outcome_unknown");
        assert!(close);
        assert_eq!(
            database
                .gateway_command("unknown")?
                .context("missing command")?
                .state,
            "outcome_unknown"
        );
        Ok(())
    }

    #[test]
    fn resume_rejections_are_safely_classified_without_thread_identifiers() {
        let operation = ControllerOperation::ThreadResume {
            thread_id: "private-thread-id".into(),
            thread_key: "private-thread-key".into(),
        };
        let active_writer = json!({"error":{"code":-32600,"message":"thread private-thread-id already has an active writer"}});
        let archived = json!({"error":{"code":-32600,"message":"session private-thread-id is archived. Run a private command"}});
        let private = json!({"error":{"code":-32000,"message":"private upstream payload"}});

        assert_eq!(
            safe_upstream_rejection(&operation, &active_writer),
            (
                "THREAD_IN_USE",
                "the selected Thread is already open in another Codex client"
            )
        );
        assert_eq!(
            safe_upstream_rejection(&operation, &archived),
            (
                "THREAD_ARCHIVED",
                "the selected Thread must be unarchived before it can be resumed"
            )
        );
        assert_eq!(
            safe_upstream_rejection(&operation, &private),
            (
                "UPSTREAM_REJECTED",
                "the App Server rejected the requested operation"
            )
        );
    }

    #[tokio::test]
    async fn unmaterialized_loaded_thread_does_not_disconnect_the_source() -> Result<()> {
        let (_temp, _database, writer) = gateway_writer()?;
        let (client_io, server_io) = duplex(8192);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let server_task = tokio::spawn(async move {
            let request = server
                .next()
                .await
                .context("missing loaded list request")??;
            let Message::Text(request) = request else {
                bail!("expected loaded list request")
            };
            let request: Value = serde_json::from_str(&request)?;
            server.send(Message::Text(json!({"id":request["id"],"result":{"data":["empty-thread"],"nextCursor":null}}).to_string().into())).await?;

            let request = server
                .next()
                .await
                .context("missing thread read request")??;
            let Message::Text(request) = request else {
                bail!("expected thread read request")
            };
            let request: Value = serde_json::from_str(&request)?;
            server.send(Message::Text(json!({"id":request["id"],"error":{"code":-32600,
                "message":"thread empty-thread is not materialized yet; includeTurns is unavailable before first user message"}}).to_string().into())).await?;
            Result::<()>::Ok(())
        });
        let mut live = session();
        discover_loaded_threads(&mut client, &writer, &mut live).await?;
        server_task.await??;
        assert!(live.attached_threads.contains("empty-thread"));
        Ok(())
    }

    #[tokio::test]
    async fn command_timeout_after_write_is_outcome_unknown_without_replay() -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        authorize_test_command(&writer, "timeout", "turn.start", Some("thread"))?;
        let (client_io, server_io) = duplex(8192);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let server_task = tokio::spawn(async move {
            let _request = server.next().await.context("missing request")??;
            tokio::time::sleep(COMMAND_RESPONSE_TIMEOUT * 2).await;
            Result::<()>::Ok(())
        });
        let mut live = session();
        live.attached_threads.insert("thread".into());
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut live,
            ActorCommand {
                command_id: "timeout".into(),
                operation: ControllerOperation::TurnStart {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    client_user_message_id: "message".into(),
                    text: "hello".into(),
                    image_paths: Vec::new(),
                },
            },
        )
        .await?;
        server_task.await??;
        assert_eq!(record.state, "outcome_unknown");
        assert!(!close);
        Ok(())
    }

    #[tokio::test]
    async fn typed_conversation_and_slice_six_operations_use_only_published_protocols() -> Result<()>
    {
        let (_temp, database, writer) = gateway_writer()?;
        let cases = vec![
            (
                "new",
                "thread.start",
                "thread/start",
                ControllerOperation::ThreadStart {
                    cwd: "/synthetic/workspace".into(),
                    model: None,
                    personality: None,
                    permissions: None,
                },
                json!({"thread":{"id":"new-thread","turns":[]}}),
            ),
            (
                "resume",
                "thread.resume",
                "thread/resume",
                ControllerOperation::ThreadResume {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                },
                json!({"thread":{"id":"thread","turns":[]}}),
            ),
            (
                "fork",
                "thread.fork",
                "thread/fork",
                ControllerOperation::ThreadFork {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    last_turn_id: Some("old-turn".into()),
                },
                json!({"thread":{"id":"forked-thread","turns":[]}}),
            ),
            (
                "steer",
                "turn.steer",
                "turn/steer",
                ControllerOperation::TurnSteer {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    expected_turn_id: "turn".into(),
                    client_user_message_id: "steer-message".into(),
                    text: "more context".into(),
                    image_paths: Vec::new(),
                },
                json!({"turnId":"turn"}),
            ),
            (
                "interrupt",
                "turn.interrupt",
                "turn/interrupt",
                ControllerOperation::TurnInterrupt {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    expected_turn_id: "turn".into(),
                },
                json!({}),
            ),
            (
                "rename",
                "thread.name.set",
                "thread/name/set",
                ControllerOperation::ThreadNameSet {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    name: "Synthetic name".into(),
                },
                json!({}),
            ),
            (
                "archive",
                "thread.archive",
                "thread/archive",
                ControllerOperation::ThreadArchive {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                },
                json!({}),
            ),
            (
                "compact",
                "thread.compact",
                "thread/compact/start",
                ControllerOperation::ThreadCompact {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                },
                json!({}),
            ),
            (
                "review",
                "review.start",
                "review/start",
                ControllerOperation::ReviewStart {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    target: crate::controller::ReviewTarget::UncommittedChanges,
                },
                json!({"reviewThreadId":"thread","turn":{"id":"review-turn","status":"completed","items":[]}}),
            ),
            (
                "goal-get",
                "thread.goal.get",
                "thread/goal/get",
                ControllerOperation::GoalGet {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                },
                json!({"goal":{"threadId":"thread","objective":"synthetic","status":"active"}}),
            ),
            (
                "goal-set",
                "thread.goal.set",
                "thread/goal/set",
                ControllerOperation::GoalSet {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    objective: Some("synthetic".into()),
                    status: Some("active".into()),
                },
                json!({"goal":{"threadId":"thread","objective":"synthetic","status":"active"}}),
            ),
            (
                "goal-clear",
                "thread.goal.clear",
                "thread/goal/clear",
                ControllerOperation::GoalClear {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                },
                json!({"cleared":true}),
            ),
        ];
        for (command_id, capability, expected_method, operation, result) in cases {
            authorize_test_command(
                &writer,
                command_id,
                capability,
                operation.thread_target().map(|(thread_id, _)| thread_id),
            )?;
            let (client_io, server_io) = duplex(16 * 1024);
            let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
            let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
            let expected_method = expected_method.to_string();
            let server_task = tokio::spawn(async move {
                let request = server.next().await.context("missing request")??;
                let Message::Text(request) = request else {
                    bail!("expected text request");
                };
                let request: Value = serde_json::from_str(&request)?;
                assert_eq!(request["method"], expected_method);
                server
                    .send(Message::Text(
                        json!({"id":request["id"],"result":result})
                            .to_string()
                            .into(),
                    ))
                    .await?;
                Result::<()>::Ok(())
            });
            let mut live = session();
            live.epoch_id = format!("epoch-{command_id}");
            live.attached_threads.insert("thread".into());
            if matches!(
                operation,
                ControllerOperation::TurnSteer { .. } | ControllerOperation::TurnInterrupt { .. }
            ) {
                live.active_turns.insert("thread".into(), "turn".into());
            }
            let (record, close) = dispatch_actor_command(
                &mut client,
                &writer,
                &database,
                &mut live,
                ActorCommand {
                    command_id: command_id.into(),
                    operation,
                },
            )
            .await?;
            server_task.await??;
            assert_eq!(record.state, "completed", "case {command_id}: {record:?}");
            assert!(!close, "case {command_id}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn stale_active_turn_rejects_steer_and_interrupt_before_upstream_write() -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        for (command_id, capability, operation) in [
            (
                "stale-steer",
                "turn.steer",
                ControllerOperation::TurnSteer {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    expected_turn_id: "stale".into(),
                    client_user_message_id: "message".into(),
                    text: "hello".into(),
                    image_paths: Vec::new(),
                },
            ),
            (
                "stale-interrupt",
                "turn.interrupt",
                ControllerOperation::TurnInterrupt {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    expected_turn_id: "stale".into(),
                },
            ),
        ] {
            authorize_test_command(&writer, command_id, capability, Some("thread"))?;
            let (client_io, server_io) = duplex(4096);
            let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
            let _server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
            let mut live = session();
            live.attached_threads.insert("thread".into());
            live.active_turns.insert("thread".into(), "active".into());
            let (record, close) = dispatch_actor_command(
                &mut client,
                &writer,
                &database,
                &mut live,
                ActorCommand {
                    command_id: command_id.into(),
                    operation,
                },
            )
            .await?;
            assert_eq!(record.state, "rejected");
            assert_eq!(
                record.error.as_ref().map(|error| error.code.as_str()),
                Some("TURN_STATE_CONFLICT")
            );
            assert!(!close);
        }
        Ok(())
    }

    fn settings_session() -> LiveSession {
        let mut live = session();
        live.attached_threads.insert("thread".into());
        live.thread_models.insert("thread".into(), "model-a".into());
        live.thread_reasoning_efforts
            .insert("thread".into(), json!("high"));
        live.capability_catalog.record_response(
            "model/list",
            false,
            &json!({"result":{"data":[
                {"id":"model-a","hidden":false,"supportsPersonality":true,
                 "supportedReasoningEfforts":[{"reasoningEffort":"high","description":"High"}]},
                {"id":"hidden-model","hidden":true,"supportsPersonality":false,
                 "supportedReasoningEfforts":[]}
            ]}}),
        );
        live.capability_catalog.record_response(
            "permissionProfile/list",
            false,
            &json!({"result":{"data":[
                {"id":":workspace","allowed":true},
                {"id":"blocked","allowed":false}
            ]}}),
        );
        live.capability_catalog.record_response(
            "collaborationMode/list",
            true,
            &json!({"result":{"data":[
                {
                    "name":"Plan",
                    "mode":"plan",
                    "model":"model-a",
                    "reasoning_effort":"high"
                },
                {
                    "name":"Default",
                    "mode":"default",
                    "model":"model-a",
                    "reasoning_effort":null
                }
            ]}}),
        );
        for (method, result) in [
            ("mcpServerStatus/list", json!({"data":[]})),
            ("account/usage/read", json!({"summary":{}})),
            ("account/rateLimits/read", json!({"rateLimits":{}})),
        ] {
            live.capability_catalog
                .record_response(method, false, &json!({"result":result}));
        }
        live
    }

    #[tokio::test]
    async fn plan_dispatch_uses_catalog_mode_for_settings_and_idle_turn() -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        for (command_id, mode, prompt, expected_method, expected_effort) in [
            (
                "plan-settings",
                "plan",
                None,
                "thread/settings/update",
                Some("high"),
            ),
            (
                "default-settings",
                "default",
                None,
                "thread/settings/update",
                Some("high"),
            ),
            (
                "plan-turn",
                "plan",
                Some("Create a plan"),
                "turn/start",
                Some("high"),
            ),
        ] {
            authorize_test_command(&writer, command_id, "thread.plan", Some("thread"))?;
            let (client_io, server_io) = duplex(16 * 1024);
            let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
            let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
            let expected_method = expected_method.to_string();
            let expected_mode = mode.to_string();
            let expected_effort = expected_effort.map(str::to_string);
            let server_task = tokio::spawn(async move {
                let request = server.next().await.context("missing Plan request")??;
                let Message::Text(request) = request else {
                    bail!("expected Plan text request");
                };
                let request: Value = serde_json::from_str(&request)?;
                assert_eq!(request["method"], expected_method);
                assert_eq!(
                    request["params"]["collaborationMode"]["mode"],
                    expected_mode
                );
                assert_eq!(
                    request["params"]["collaborationMode"]["settings"],
                    json!({
                        "model":"model-a",
                        "reasoning_effort":expected_effort,
                        "developer_instructions":null,
                    })
                );
                let result = if expected_method == "turn/start" {
                    json!({"turn":{"id":"plan-turn","status":"completed","items":[]}})
                } else {
                    json!({})
                };
                server
                    .send(Message::Text(
                        json!({"id":request["id"],"result":result})
                            .to_string()
                            .into(),
                    ))
                    .await?;
                Result::<()>::Ok(())
            });
            let mut live = settings_session();
            live.epoch_id = format!("epoch-{command_id}");
            let (record, close) = dispatch_actor_command(
                &mut client,
                &writer,
                &database,
                &mut live,
                ActorCommand {
                    command_id: command_id.into(),
                    operation: ControllerOperation::Plan {
                        thread_id: "thread".into(),
                        thread_key: thread_key("store-source", "thread"),
                        mode: mode.into(),
                        expected_turn_id: None,
                        client_user_message_id: prompt.map(|_| "plan-message".into()),
                        prompt: prompt.map(str::to_string),
                    },
                },
            )
            .await?;
            server_task.await??;
            assert_eq!(record.state, "completed");
            if prompt.is_none() {
                assert_eq!(
                    record
                        .result
                        .as_ref()
                        .and_then(|value| value["collaborationMode"].as_str()),
                    Some(mode)
                );
            }
            assert!(!close);
        }
        Ok(())
    }

    #[tokio::test]
    async fn leaving_plan_restores_the_pre_plan_reasoning_effort() -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        let mut live = settings_session();
        live.thread_reasoning_efforts
            .insert("thread".into(), json!("low"));
        live.capability_catalog.record_response(
            "model/list",
            false,
            &json!({"result":{"data":[{
                "id":"model-a",
                "hidden":false,
                "supportsPersonality":true,
                "supportedReasoningEfforts":[
                    {"reasoningEffort":"low","description":"Low"},
                    {"reasoningEffort":"high","description":"High"}
                ]
            }]}}),
        );

        authorize_test_command(&writer, "enter-plan", "thread.plan", Some("thread"))?;
        let (client_io, server_io) = duplex(16 * 1024);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let server_task = tokio::spawn(async move {
            let request = server
                .next()
                .await
                .context("missing enter Plan request")??;
            let Message::Text(request) = request else {
                bail!("expected enter Plan text request");
            };
            let request: Value = serde_json::from_str(&request)?;
            assert_eq!(
                request["params"]["collaborationMode"]["settings"]["reasoning_effort"],
                "high"
            );
            server
                .send(Message::Text(
                    json!({
                        "method":"thread/settings/updated",
                        "params":{
                            "threadId":"thread",
                            "threadSettings":{
                                "model":"model-a",
                                "effort":"high",
                                "collaborationMode":request["params"]["collaborationMode"].clone()
                            }
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await?;
            server
                .send(Message::Text(
                    json!({"id":request["id"],"result":{}}).to_string().into(),
                ))
                .await?;
            Result::<()>::Ok(())
        });
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut live,
            ActorCommand {
                command_id: "enter-plan".into(),
                operation: ControllerOperation::Plan {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    mode: "plan".into(),
                    expected_turn_id: None,
                    client_user_message_id: None,
                    prompt: None,
                },
            },
        )
        .await?;
        server_task.await??;
        assert_eq!(record.state, "completed");
        assert!(!close);
        assert_eq!(live.thread_reasoning_efforts["thread"], "high");
        assert_eq!(
            live.thread_default_collaboration_modes["thread"]["settings"]["reasoning_effort"],
            "low"
        );

        authorize_test_command(&writer, "exit-plan", "thread.plan", Some("thread"))?;
        let (client_io, server_io) = duplex(16 * 1024);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let server_task = tokio::spawn(async move {
            let request = server.next().await.context("missing exit Plan request")??;
            let Message::Text(request) = request else {
                bail!("expected exit Plan text request");
            };
            let request: Value = serde_json::from_str(&request)?;
            assert_eq!(request["params"]["collaborationMode"]["mode"], "default");
            assert_eq!(
                request["params"]["collaborationMode"]["settings"]["reasoning_effort"],
                "low"
            );
            server
                .send(Message::Text(
                    json!({"id":request["id"],"result":{}}).to_string().into(),
                ))
                .await?;
            Result::<()>::Ok(())
        });
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut live,
            ActorCommand {
                command_id: "exit-plan".into(),
                operation: ControllerOperation::Plan {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    mode: "default".into(),
                    expected_turn_id: None,
                    client_user_message_id: None,
                    prompt: None,
                },
            },
        )
        .await?;
        server_task.await??;
        assert_eq!(record.state, "completed");
        assert!(!close);
        Ok(())
    }

    #[tokio::test]
    async fn active_plan_applies_mode_then_steers_and_partial_rejection_is_unknown() -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        for (command_id, reject_steer, expected_state) in [
            ("plan-steer", false, "completed"),
            ("plan-steer-rejected", true, "outcome_unknown"),
        ] {
            authorize_test_command(&writer, command_id, "thread.plan", Some("thread"))?;
            let (client_io, server_io) = duplex(16 * 1024);
            let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
            let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
            let server_task = tokio::spawn(async move {
                let settings = server.next().await.context("missing Plan settings")??;
                let Message::Text(settings) = settings else {
                    bail!("expected settings text request");
                };
                let settings: Value = serde_json::from_str(&settings)?;
                assert_eq!(settings["method"], "thread/settings/update");
                server
                    .send(Message::Text(
                        json!({"id":settings["id"],"result":{}}).to_string().into(),
                    ))
                    .await?;
                let steer = server.next().await.context("missing Plan steer")??;
                let Message::Text(steer) = steer else {
                    bail!("expected steer text request");
                };
                let steer: Value = serde_json::from_str(&steer)?;
                assert_eq!(steer["method"], "turn/steer");
                assert_eq!(steer["params"]["expectedTurnId"], "turn");
                assert_eq!(steer["params"]["input"][0]["text"], "Refine the plan");
                let response = if reject_steer {
                    json!({"id":steer["id"],"error":{"code":-32000,"message":"private rejection"}})
                } else {
                    json!({"id":steer["id"],"result":{"turnId":"turn"}})
                };
                server
                    .send(Message::Text(response.to_string().into()))
                    .await?;
                Result::<()>::Ok(())
            });
            let mut live = settings_session();
            live.epoch_id = format!("epoch-{command_id}");
            live.active_turns.insert("thread".into(), "turn".into());
            let (record, close) = dispatch_actor_command(
                &mut client,
                &writer,
                &database,
                &mut live,
                ActorCommand {
                    command_id: command_id.into(),
                    operation: ControllerOperation::Plan {
                        thread_id: "thread".into(),
                        thread_key: thread_key("store-source", "thread"),
                        mode: "plan".into(),
                        expected_turn_id: Some("turn".into()),
                        client_user_message_id: Some("plan-steer-message".into()),
                        prompt: Some("Refine the plan".into()),
                    },
                },
            )
            .await?;
            server_task.await??;
            assert_eq!(record.state, expected_state);
            assert!(!close);
            if reject_steer {
                assert_eq!(
                    record.error.as_ref().map(|error| error.code.as_str()),
                    Some("OUTCOME_UNKNOWN")
                );
                assert!(!format!("{record:?}").contains("private rejection"));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn plan_is_rejected_before_write_when_experimental_catalog_is_unavailable() -> Result<()>
    {
        let (_temp, database, writer) = gateway_writer()?;
        authorize_test_command(&writer, "plan-unavailable", "thread.plan", Some("thread"))?;
        let (client_io, server_io) = duplex(4096);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let _server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let mut live = settings_session();
        live.capability_catalog.record_response(
            "collaborationMode/list",
            true,
            &json!({"error":{"code":-32601,"message":"private unavailable detail"}}),
        );
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut live,
            ActorCommand {
                command_id: "plan-unavailable".into(),
                operation: ControllerOperation::Plan {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    mode: "plan".into(),
                    expected_turn_id: None,
                    client_user_message_id: None,
                    prompt: None,
                },
            },
        )
        .await?;
        assert_eq!(record.state, "rejected");
        assert_eq!(
            record.error.as_ref().map(|error| error.code.as_str()),
            Some("CAPABILITY_UNAVAILABLE")
        );
        assert!(!close);
        assert!(!format!("{record:?}").contains("private unavailable detail"));
        Ok(())
    }

    #[tokio::test]
    async fn plan_protocol_rejection_is_sanitized_without_claiming_mode_change() -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        authorize_test_command(&writer, "plan-rejected", "thread.plan", Some("thread"))?;
        let (client_io, server_io) = duplex(8192);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let server_task = tokio::spawn(async move {
            let request = server.next().await.context("missing Plan request")??;
            let Message::Text(request) = request else {
                bail!("expected Plan text request");
            };
            let request: Value = serde_json::from_str(&request)?;
            server
                .send(Message::Text(
                    json!({
                        "id":request["id"],
                        "error":{"code":-32000,"message":"private Plan rejection"}
                    })
                    .to_string()
                    .into(),
                ))
                .await?;
            Result::<()>::Ok(())
        });
        let mut live = settings_session();
        let (record, close) = dispatch_actor_command(
            &mut client,
            &writer,
            &database,
            &mut live,
            ActorCommand {
                command_id: "plan-rejected".into(),
                operation: ControllerOperation::Plan {
                    thread_id: "thread".into(),
                    thread_key: thread_key("store-source", "thread"),
                    mode: "plan".into(),
                    expected_turn_id: None,
                    client_user_message_id: None,
                    prompt: None,
                },
            },
        )
        .await?;
        server_task.await??;
        assert_eq!(record.state, "rejected");
        assert_eq!(
            record.error.as_ref().map(|error| error.code.as_str()),
            Some("UPSTREAM_REJECTED")
        );
        assert!(!close);
        assert!(live.thread_collaboration_modes.is_empty());
        assert!(!format!("{record:?}").contains("private Plan rejection"));
        Ok(())
    }

    #[test]
    fn slash_catalog_is_source_thread_and_capability_aware() {
        let mut live = settings_session();
        live.active_turns.insert("thread".into(), "turn".into());
        let catalog = live.control_catalog(Some("thread"));
        let names = catalog
            .slash_commands
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<BTreeSet<_>>();
        assert!(catalog.thread_loaded);
        assert_eq!(catalog.active_turn_id.as_deref(), Some("turn"));
        for name in [
            "/new",
            "/fork",
            "/model",
            "/reasoning",
            "/personality",
            "/permissions",
            "/interrupt",
            "/plan",
            "/mcp",
            "/usage",
        ] {
            assert!(names.contains(name), "missing {name}");
        }
        assert!(!names.contains("/resume"));
        assert!(!names.contains("/clear"));

        let unloaded = live.control_catalog(Some("other"));
        let names = unloaded
            .slash_commands
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<BTreeSet<_>>();
        assert!(names.contains("/resume"));
        assert!(!names.contains("/model"));
        assert!(!names.contains("/interrupt"));

        live.capability_catalog.record_response(
            "collaborationMode/list",
            true,
            &json!({"error":{"code":-32601,"message":"not available"}}),
        );
        live.capability_catalog.record_response(
            "mcpServerStatus/list",
            false,
            &json!({"error":{"code":-32601,"message":"not available"}}),
        );
        live.capability_catalog.record_response(
            "account/usage/read",
            false,
            &json!({"error":{"code":-32601,"message":"not available"}}),
        );
        let unavailable = live.control_catalog(Some("thread"));
        let names = unavailable
            .slash_commands
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<BTreeSet<_>>();
        assert!(!names.contains("/plan"));
        assert!(!names.contains("/mcp"));
        assert!(!names.contains("/usage"));

        live.active_turns.remove("thread");
        assert!(
            live.control_catalog(Some("thread"))
                .slash_commands
                .iter()
                .any(|entry| entry.name == "/clear")
        );
        for method in [
            "thread/name/set",
            "thread/archive",
            "thread/compact/start",
            "review/start",
            "thread/settings/update",
            "thread/goal/get",
        ] {
            live.capability_catalog.record_availability(
                method,
                false,
                &json!({"error":{"code":-32601,"message":"not found"}}),
            );
        }
        let unavailable = live.control_catalog(Some("thread"));
        let names = unavailable
            .slash_commands
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<BTreeSet<_>>();
        for name in [
            "/rename",
            "/archive",
            "/compact",
            "/review",
            "/model",
            "/reasoning",
            "/personality",
            "/permissions",
            "/goal",
        ] {
            assert!(!names.contains(name), "unexpected unavailable {name}");
        }
    }

    #[tokio::test]
    async fn settings_dispatch_uses_catalog_gated_official_update_fields() -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        for (command_id, capability, setting, expected_key, expected_value) in [
            (
                "setting-model",
                "thread.settings.model",
                ThreadSetting::Model("model-a".into()),
                "model",
                "model-a",
            ),
            (
                "setting-reasoning",
                "thread.settings.reasoning",
                ThreadSetting::ReasoningEffort("high".into()),
                "effort",
                "high",
            ),
            (
                "setting-personality",
                "thread.settings.personality",
                ThreadSetting::Personality("pragmatic".into()),
                "personality",
                "pragmatic",
            ),
            (
                "setting-permissions",
                "thread.settings.permissions",
                ThreadSetting::Permissions(":workspace".into()),
                "permissions",
                ":workspace",
            ),
        ] {
            authorize_test_command(&writer, command_id, capability, Some("thread"))?;
            let (client_io, server_io) = duplex(8192);
            let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
            let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
            let expected_key = expected_key.to_string();
            let expected_value = expected_value.to_string();
            let server_task = tokio::spawn(async move {
                let request = server.next().await.context("missing settings request")??;
                let Message::Text(request) = request else {
                    bail!("expected text request");
                };
                let request: Value = serde_json::from_str(&request)?;
                assert_eq!(request["method"], "thread/settings/update");
                assert_eq!(request["params"]["threadId"], "thread");
                assert_eq!(request["params"][&expected_key], expected_value);
                assert_eq!(
                    request["params"].as_object().map(|value| value.len()),
                    Some(2)
                );
                server
                    .send(Message::Text(
                        json!({"id":request["id"],"result":{}}).to_string().into(),
                    ))
                    .await?;
                Result::<()>::Ok(())
            });
            let mut live = settings_session();
            live.epoch_id = format!("epoch-{command_id}");
            let (record, close) = dispatch_actor_command(
                &mut client,
                &writer,
                &database,
                &mut live,
                ActorCommand {
                    command_id: command_id.into(),
                    operation: ControllerOperation::ThreadSettingsUpdate {
                        thread_id: "thread".into(),
                        thread_key: thread_key("store-source", "thread"),
                        setting,
                    },
                },
            )
            .await?;
            server_task.await??;
            assert_eq!(record.state, "completed");
            assert!(!close);
        }
        Ok(())
    }

    #[tokio::test]
    async fn settings_reject_hidden_unsupported_and_disallowed_catalog_values() -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        for (command_id, capability, setting) in [
            (
                "hidden-model",
                "thread.settings.model",
                ThreadSetting::Model("hidden-model".into()),
            ),
            (
                "unsupported-effort",
                "thread.settings.reasoning",
                ThreadSetting::ReasoningEffort("impossible".into()),
            ),
            (
                "blocked-profile",
                "thread.settings.permissions",
                ThreadSetting::Permissions("blocked".into()),
            ),
        ] {
            authorize_test_command(&writer, command_id, capability, Some("thread"))?;
            let (client_io, server_io) = duplex(4096);
            let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
            let _server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
            let (record, close) = dispatch_actor_command(
                &mut client,
                &writer,
                &database,
                &mut settings_session(),
                ActorCommand {
                    command_id: command_id.into(),
                    operation: ControllerOperation::ThreadSettingsUpdate {
                        thread_id: "thread".into(),
                        thread_key: thread_key("store-source", "thread"),
                        setting,
                    },
                },
            )
            .await?;
            assert_eq!(record.state, "rejected");
            assert_eq!(
                record.error.as_ref().map(|error| error.code.as_str()),
                Some("CAPABILITY_UNAVAILABLE")
            );
            assert!(!close);
        }
        Ok(())
    }

    #[test]
    fn server_request_is_persisted_but_never_answered() -> Result<()> {
        let request = normalize_envelope(
            &session(),
            &json!({"method":"item/commandExecution/requestApproval","id":7,"params":{"threadId":"thread","turnId":"turn","itemId":"item"}}),
            None,
            None,
        )?;
        assert_eq!(request.phase, "request");
        assert_eq!(request.request_id.as_deref(), Some("7"));
        assert_eq!(request.item_type.as_deref(), Some("approval"));
        Ok(())
    }

    #[test]
    fn malformed_protocol_envelopes_fail_closed_but_unknown_methods_are_valid() -> Result<()> {
        assert!(validate_envelope(&json!({"method":7,"params":{}})).is_err());
        assert!(validate_envelope(&json!({"id":1})).is_err());
        assert!(validate_envelope(&json!({"method":"future/method","params":{"x":1}})).is_ok());
        Ok(())
    }

    #[test]
    fn projects_live_lifecycle_and_pending_request() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        database.upsert_source_kind(
            "app-source",
            "app_server",
            "/tmp/socket",
            &json!({}),
            "ready",
        )?;
        let mut session = session();
        session.source_seq = 0;
        ingest_envelope(
            &database,
            &mut session,
            &json!({"method":"turn/started","params":{"threadId":"thread","turn":{"id":"turn"}}}),
            None,
            None,
        )?;
        ingest_envelope(
            &database,
            &mut session,
            &json!({"method":"thread/status/changed","params":{"threadId":"thread","status":{"type":"active"}}}),
            None,
            None,
        )?;
        let active_status: String = database.connect()?.query_row(
            "SELECT runtime_status FROM threads WHERE codex_thread_id='thread'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(active_status, "active");
        for delta in ["hel", "lo"] {
            ingest_envelope(
                &database,
                &mut session,
                &json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","itemId":"message","delta":delta}}),
                None,
                None,
            )?;
        }
        ingest_envelope(
            &database,
            &mut session,
            &json!({"method":"item/commandExecution/requestApproval","id":7,"params":{"threadId":"thread","turnId":"turn","itemId":"item"}}),
            None,
            None,
        )?;
        ingest_envelope(
            &database,
            &mut session,
            &json!({"method":"serverRequest/resolved","params":{"threadId":"thread","requestId":"7"}}),
            None,
            None,
        )?;
        ingest_envelope(
            &database,
            &mut session,
            &json!({"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn","status":"completed"}}}),
            None,
            None,
        )?;
        let connection = database.connect()?;
        let completeness: String = connection.query_row(
            "SELECT capture_completeness FROM turns WHERE turn_id='turn'",
            [],
            |row| row.get(0),
        )?;
        let pending: (String, String) = connection.query_row(
            "SELECT request_type,state FROM pending_requests WHERE request_id='7'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let transient: i64 = connection.query_row(
            "SELECT COUNT(*) FROM raw_events WHERE durability='transient'",
            [],
            |row| row.get(0),
        )?;
        let runtime_status: String = connection.query_row(
            "SELECT runtime_status FROM threads WHERE codex_thread_id='thread'",
            [],
            |row| row.get(0),
        )?;
        let (store_source_id, message): (String, String) = connection.query_row(
            "SELECT t.store_source_id,i.summary_text FROM threads t JOIN items i USING(thread_key)
             WHERE t.codex_thread_id='thread' AND i.item_id='message'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(completeness, "live_complete");
        assert_eq!(pending, ("approval".into(), "resolved".into()));
        assert_eq!(transient, 7);
        assert_eq!(runtime_status, "idle");
        assert_eq!(store_source_id, "store-source");
        assert_eq!(message, "hello");
        drop(connection);
        assert_eq!(database.rebuild_projections()?, 7);
        let rebuilt: (String, String) = database.connect()?.query_row(
            "SELECT t.store_source_id,i.summary_text FROM threads t JOIN items i USING(thread_key)
             WHERE t.codex_thread_id='thread' AND i.item_id='message'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(rebuilt, ("store-source".into(), "hello".into()));
        Ok(())
    }

    #[test]
    fn projects_official_thread_settings_notification() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        database.upsert_source_kind(
            "app-source",
            "app_server",
            "/tmp/socket",
            &json!({}),
            "ready",
        )?;
        let mut live = session();
        live.source_seq = 0;
        ingest_envelope(
            &database,
            &mut live,
            &json!({
                "method":"thread/settings/updated",
                "params":{
                    "threadId":"thread",
                    "threadSettings":{
                        "cwd":"/synthetic/workspace",
                        "model":"model-a",
                        "effort":"high",
                        "approvalPolicy":"on-request",
                        "activePermissionProfile":{"id":":workspace"}
                    }
                }
            }),
            None,
            None,
        )?;
        let projected: (String, String, String, String) = database.connect()?.query_row(
            "SELECT cwd,model,reasoning_effort,active_permission_profile_json FROM threads WHERE codex_thread_id='thread'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(projected.0, "/synthetic/workspace");
        assert_eq!(projected.1, "model-a");
        assert_eq!(projected.2, "high");
        assert!(projected.3.contains(":workspace"));
        Ok(())
    }

    #[test]
    fn goal_projection_and_actor_cache_rebuild_from_get_and_clear_events() -> Result<()> {
        let (_temp, database, writer) = gateway_writer()?;
        let mut live = session();
        live.source_seq = 0;
        let goal = json!({
            "threadId":"thread",
            "objective":"Synthetic reconnect objective",
            "status":"paused",
            "tokensUsed":10,
            "timeUsedSeconds":20,
            "createdAt":1,
            "updatedAt":2,
            "tokenBudget":100,
        });
        let response = json!({"id":41,"result":{"goal":goal}});
        let observation = ingest_envelope(
            &writer,
            &mut live,
            &response,
            Some("thread/goal/get/response"),
            Some("thread"),
        )?;
        apply_control_observation(&writer, &mut live, observation)?;
        assert_eq!(live.thread_goals["thread"]["status"], "paused");
        let projected: (String, String) = database.connect()?.query_row(
            "SELECT status,goal_json FROM thread_goals",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(projected.0, "paused");
        assert!(projected.1.contains("Synthetic reconnect objective"));

        let cleared = json!({"id":42,"result":{"cleared":true}});
        let observation = ingest_envelope(
            &writer,
            &mut live,
            &cleared,
            Some("thread/goal/clear/response"),
            Some("thread"),
        )?;
        apply_control_observation(&writer, &mut live, observation)?;
        assert!(!live.thread_goals.contains_key("thread"));
        assert_eq!(
            database
                .connect()?
                .query_row("SELECT COUNT(*) FROM thread_goals", [], |row| row
                    .get::<_, i64>(0))?,
            0
        );
        assert_eq!(database.rebuild_projections()?, 2);
        assert_eq!(
            database
                .connect()?
                .query_row("SELECT COUNT(*) FROM thread_goals", [], |row| row
                    .get::<_, i64>(0))?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn reconnect_goal_refresh_restores_cache_without_exposing_objective_in_catalog()
    -> Result<()> {
        let (_temp, _database, writer) = gateway_writer()?;
        let (client_io, server_io) = duplex(8192);
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let server_task = tokio::spawn(async move {
            let request = server.next().await.context("missing goal refresh")??;
            let Message::Text(request) = request else {
                bail!("expected goal refresh text request");
            };
            let request: Value = serde_json::from_str(&request)?;
            assert_eq!(request["method"], "thread/goal/get");
            server
                .send(Message::Text(
                    json!({
                        "id":request["id"],
                        "result":{"goal":{
                            "threadId":"thread",
                            "objective":"Private reconnect objective",
                            "status":"active"
                        }}
                    })
                    .to_string()
                    .into(),
                ))
                .await?;
            Result::<()>::Ok(())
        });
        let mut reconnected = session();
        reconnected.epoch_id = "reconnected-epoch".into();
        refresh_thread_goal(&mut client, &writer, &mut reconnected, "thread").await?;
        server_task.await??;
        assert_eq!(
            reconnected.thread_goals["thread"]["objective"],
            "Private reconnect objective"
        );
        let capability = &reconnected.capability_catalog.entries["thread/goal/get"];
        assert!(capability.available);
        assert!(capability.data.is_none());
        assert!(
            !reconnected
                .capability_catalog
                .summary()
                .to_string()
                .contains("Private reconnect objective")
        );
        Ok(())
    }

    #[test]
    fn durable_item_wins_and_records_projection_conflict() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        database.upsert_source_kind(
            "app-source",
            "app_server",
            "/tmp/socket",
            &json!({}),
            "ready",
        )?;
        database.upsert_source("store-source", "/tmp/store", &json!({}), "ready")?;
        let mut session = session();
        session.source_seq = 0;
        ingest_envelope(
            &database,
            &mut session,
            &json!({"method":"item/completed","params":{"threadId":"thread","turnId":"turn","item":{"id":"item","type":"agentMessage","content":[{"text":"live value"}]}}}),
            None,
            None,
        )?;
        let mut durable = normalize_envelope(
            &session,
            &json!({"method":"item/completed","params":{"threadId":"thread","turnId":"turn","item":{"id":"item","type":"agentMessage","content":[{"text":"durable value"}]}}}),
            None,
            None,
        )?;
        durable.event_id = "durable-event".into();
        durable.source_id = "store-source".into();
        durable.epoch_id = "rollout-epoch".into();
        durable.dedupe_key = "durable-item".into();
        durable.durability = "durable".into();
        durable.top_type = "response_item".into();
        durable.raw_json = json!({"type":"response_item","payload":durable.payload}).to_string();
        durable.stored_raw_hash = blake3::hash(durable.raw_json.as_bytes())
            .to_hex()
            .to_string();
        database.ingest_batch(&OwnedIngestBatch {
            source_id: "store-source".into(),
            epoch_id: "rollout-epoch".into(),
            checkpoint_key: "rollout-conflict".into(),
            file_identity: "fixture".into(),
            byte_offset: 1,
            ordinal: 1,
            current_turn_id: Some("turn".into()),
            clean_eof: false,
            events: vec![durable],
        })?;
        let connection = database.connect()?;
        let summary: String = connection.query_row(
            "SELECT summary_text FROM items WHERE item_id='item'",
            [],
            |row| row.get(0),
        )?;
        let conflicts: i64 = connection.query_row(
            "SELECT COUNT(*) FROM projection_conflicts WHERE status='active'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(summary, "durable value");
        assert!(conflicts >= 1);
        drop(connection);
        assert_eq!(database.rebuild_projections()?, 2);
        assert!(
            database.connect()?.query_row(
                "SELECT COUNT(*) FROM projection_conflicts",
                [],
                |row| row.get::<_, i64>(0)
            )? >= 1
        );
        Ok(())
    }

    #[test]
    fn transient_delta_uses_independent_retention_window() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        database.upsert_source_kind(
            "app-source",
            "app_server",
            "/tmp/socket",
            &json!({}),
            "ready",
        )?;
        let mut session = session();
        session.source_seq = 0;
        ingest_envelope(
            &database,
            &mut session,
            &json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","itemId":"item","delta":"old delta"}}),
            None,
            None,
        )?;
        ingest_envelope(
            &database,
            &mut session,
            &json!({"method":"item/completed","params":{"threadId":"thread","turnId":"turn","item":{"id":"item","type":"agentMessage","content":[{"text":"complete"}]}}}),
            None,
            None,
        )?;
        database.connect()?.execute(
            "UPDATE raw_events SET observed_at_ms=?1",
            [now_ms() - 2 * 86_400_000],
        )?;
        let report = database.run_retention(30, 1, 30, true)?;
        assert_eq!(report.deleted_raw_events, 1);
        let remaining: String =
            database
                .connect()?
                .query_row("SELECT phase FROM raw_events", [], |row| row.get(0))?;
        assert_eq!(remaining, "completed");
        Ok(())
    }
}
