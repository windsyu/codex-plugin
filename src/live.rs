use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::{SinkExt, StreamExt};
use rand::Rng;
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{WebSocketStream, client_async};
use uuid::Uuid;

use crate::config::{Config, SourceConfig};
use crate::db::{Database, IngestBatch, classify_live_item, live_summary, now_ms};
use crate::ingest::{load_or_create_key, stable_source_id, thread_key};
use crate::model::NormalizedEvent;
use crate::redact;

struct LiveSession {
    app_source_id: String,
    store_source_id: String,
    epoch_id: String,
    socket_path: PathBuf,
    source_seq: i64,
    fingerprint_key: [u8; 32],
    attached_threads: BTreeSet<String>,
}

pub fn spawn_enabled(
    config: &Config,
    database: Arc<Database>,
    shutdown: watch::Receiver<bool>,
) -> Result<Vec<tokio::task::JoinHandle<()>>> {
    let fingerprint_key = load_or_create_key(&config.storage.fingerprint_key_file)?;
    let mut handles = Vec::new();
    for source in config
        .sources
        .iter()
        .filter(|source| source.live_mode != "off")
        .cloned()
    {
        let database = database.clone();
        let shutdown = shutdown.clone();
        handles.push(tokio::spawn(async move {
            run_with_reconnect(source, database, fingerprint_key, shutdown).await;
        }));
    }
    Ok(handles)
}

async fn run_with_reconnect(
    source: SourceConfig,
    database: Arc<Database>,
    fingerprint_key: [u8; 32],
    mut shutdown: watch::Receiver<bool>,
) {
    let socket_path = source
        .app_server_socket
        .clone()
        .expect("validated live source has a socket");
    let app_source_id = app_source_id(&socket_path);
    let stable_identity = socket_path.to_string_lossy().to_string();
    if let Err(error) = database.upsert_source_kind(
        &app_source_id,
        "app_server",
        &stable_identity,
        &json!({"name":source.name,"socket":stable_identity,"liveMode":source.live_mode}),
        "discovering",
    ) {
        tracing::error!(source_id = %app_source_id, error = %error, "register live source failed");
        return;
    }

    let mut backoff_ms = 500_u64;
    loop {
        if *shutdown.borrow() {
            let _ = database.update_source_status(&app_source_id, "paused", None);
            return;
        }
        let epoch_id = Uuid::new_v4().to_string();
        let result = connect_once(
            &source,
            &database,
            LiveSession {
                app_source_id: app_source_id.clone(),
                store_source_id: stable_source_id(&source.codex_home.to_string_lossy()),
                epoch_id: epoch_id.clone(),
                socket_path: socket_path.clone(),
                source_seq: 0,
                fingerprint_key,
                attached_threads: BTreeSet::new(),
            },
            shutdown.clone(),
        )
        .await;
        if *shutdown.borrow() {
            let _ = database.close_live_epoch(&app_source_id, &epoch_id, "graceful_shutdown");
            let _ = database.update_source_status(&app_source_id, "paused", None);
            return;
        }
        let reason = result
            .as_ref()
            .err()
            .map(|error| format!("{error:#}"))
            .unwrap_or_else(|| "connection_closed".into());
        if let Err(error) = database.close_live_epoch(&app_source_id, &epoch_id, &reason) {
            tracing::warn!(source_id = %app_source_id, error = %error, "close live epoch failed");
        }
        let _ = database.update_source_status(&app_source_id, "degraded", Some(&reason));
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
    database: &Database,
    mut session: LiveSession,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    validate_socket(&session.socket_path)?;
    database.update_source_status(&session.app_source_id, "connecting", None)?;
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
                "params":{"clientInfo":{"name":"codex_local_observer","title":"Codex Local Observer","version":env!("CARGO_PKG_VERSION")},
                "capabilities":{"experimentalApi":false}}
            })
            .to_string()
            .into(),
        ))
        .await?;
    let initialize = wait_for_response(
        &mut websocket,
        database,
        &mut session,
        json!(1),
        "initialize/response",
        None,
    )
    .await?;
    let result = initialize
        .get("result")
        .context("initialize response is missing result")?;
    let returned_home = result
        .get("codexHome")
        .and_then(Value::as_str)
        .context("initialize response is missing codexHome")?;
    if canonical_existing(Path::new(returned_home))? != canonical_existing(&source.codex_home)? {
        bail!("app-server codexHome does not match configured source");
    }
    database.record_live_capabilities(&session.app_source_id, &session.epoch_id, result)?;
    websocket
        .send(Message::Text(
            json!({"method":"initialized"}).to_string().into(),
        ))
        .await?;
    database.update_source_status(&session.app_source_id, "ready", None)?;
    tracing::info!(source_id = %session.app_source_id, mode = %source.live_mode, "live source initialized");

    if source.live_mode == "attach_loaded" {
        attach_loaded_threads(&mut websocket, database, &mut session).await?;
    }

    loop {
        let message = tokio::select! {
            _ = wait_for_shutdown(&mut shutdown) => {
                unsubscribe_attached(&mut websocket, &mut session).await?;
                return Ok(());
            }
            message = websocket.next() => message,
        };
        let Some(message) = message else { break };
        match message? {
            Message::Text(text) => {
                let envelope: Value = serde_json::from_str(&text)?;
                ingest_envelope(database, &mut session, &envelope, None, None)?;
            }
            Message::Close(_) => break,
            Message::Ping(payload) => websocket.send(Message::Pong(payload)).await?,
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
    Ok(())
}

async fn attach_loaded_threads(
    websocket: &mut WebSocketStream<UnixStream>,
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
            database,
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
                database,
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

async fn wait_for_response(
    websocket: &mut WebSocketStream<UnixStream>,
    database: &Database,
    session: &mut LiveSession,
    expected_id: Value,
    response_method: &str,
    thread_hint: Option<&str>,
) -> Result<Value> {
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
                    ingest_envelope(
                        database,
                        session,
                        &envelope,
                        Some(response_method),
                        thread_hint,
                    )?;
                    if let Some(error) = envelope.get("error") {
                        bail!("app-server request failed: {error}");
                    }
                    return Ok(envelope);
                }
                ingest_envelope(database, session, &envelope, None, None)?;
            }
            Message::Ping(payload) => websocket.send(Message::Pong(payload)).await?,
            Message::Close(_) => bail!("app-server closed before response"),
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

fn ingest_envelope(
    database: &Database,
    session: &mut LiveSession,
    envelope: &Value,
    method_hint: Option<&str>,
    thread_hint: Option<&str>,
) -> Result<()> {
    session.source_seq += 1;
    let event = normalize_envelope(session, envelope, method_hint, thread_hint)?;
    if event.method == "thread/started" && !event.codex_thread_id.is_empty() {
        session
            .attached_threads
            .insert(event.codex_thread_id.clone());
    }
    let current_turn_id = event.turn_id.clone();
    let events = [event];
    database.ingest_batch(IngestBatch {
        source_id: &session.app_source_id,
        epoch_id: &session.epoch_id,
        checkpoint_key: &format!("live:{}:{}", session.app_source_id, session.epoch_id),
        file_identity: &session.socket_path.to_string_lossy(),
        byte_offset: 0,
        ordinal: session.source_seq as u64,
        current_turn_id: current_turn_id.as_deref(),
        events: &events,
    })?;
    Ok(())
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
    let (redacted, redaction_audit) = redact::redact(envelope);
    let stored = serde_json::to_string(&redacted)?;
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
    let payload = item
        .clone()
        .or_else(|| params.get("turn").cloned())
        .or_else(|| params.get("thread").cloned())
        .unwrap_or_else(|| params.clone());
    let request = redacted.get("method").is_some() && redacted.get("id").is_some();
    let request_id = request.then(|| id_string(redacted.get("id"))).flatten();
    let item_id =
        extract_string(&params, &["/itemId", "/item/id", "/callId"]).or_else(|| request_id.clone());
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
    }
    .to_string();
    let item_type = classify_live_item(&method, item.as_ref());
    let summary = live_summary(&params, item.as_ref());
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

fn app_source_id(path: &Path) -> String {
    URL_SAFE_NO_PAD
        .encode(blake3::hash(format!("app-server\0{}", path.display()).as_bytes()).as_bytes())
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
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect app-server socket {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        bail!("configured app-server endpoint is not a direct Unix socket");
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        bail!("configured app-server socket is not owned by the current user");
    }
    if metadata.mode() & 0o077 != 0 {
        bail!("configured app-server socket permissions are broader than 0600");
    }
    let parent = path.parent().context("app-server socket has no parent")?;
    let parent_metadata = fs::metadata(parent)?;
    if parent_metadata.uid() != unsafe { libc::geteuid() } || parent_metadata.mode() & 0o022 != 0 {
        bail!("configured app-server socket directory is not private");
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_socket(_path: &Path) -> Result<()> {
    bail!("App Server live mode is currently supported on Unix only")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn session() -> LiveSession {
        LiveSession {
            app_source_id: "app-source".into(),
            store_source_id: "store-source".into(),
            epoch_id: "epoch".into(),
            socket_path: PathBuf::from("/tmp/socket"),
            source_seq: 1,
            fingerprint_key: [1_u8; 32],
            attached_threads: BTreeSet::new(),
        }
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
        let pending: String = connection.query_row(
            "SELECT request_type FROM pending_requests WHERE state='pending'",
            [],
            |row| row.get(0),
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
        assert_eq!(pending, "approval");
        assert_eq!(transient, 6);
        assert_eq!(runtime_status, "idle");
        assert_eq!(store_source_id, "store-source");
        assert_eq!(message, "hello");
        drop(connection);
        assert_eq!(database.rebuild_projections()?, 6);
        let rebuilt: (String, String) = database.connect()?.query_row(
            "SELECT t.store_source_id,i.summary_text FROM threads t JOIN items i USING(thread_key)
             WHERE t.codex_thread_id='thread' AND i.item_id='message'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(rebuilt, ("store-source".into(), "hello".into()));
        Ok(())
    }
}
