use std::convert::Infallible;
use std::fs;
use std::io::{ErrorKind, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Extension, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self as axum_middleware, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
#[cfg(test)]
use base64::Engine;
#[cfg(test)]
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::{SinkExt, StreamExt};
use rusqlite::OptionalExtension;
use rusqlite::types::Value as SqlValue;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, broadcast, watch};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

mod auth;
mod cursor;
mod handlers;
mod middleware;
mod query;
mod router;
mod stream;

use crate::config::Config;
use crate::controller::{
    ActorCommand, ControllerOperation, ControllerRegistry, PendingRequestAction, RegistryError,
    ReviewTarget, ThreadSetting,
};
use crate::credentials::{load_or_create_key, rotate_token};
use crate::domain::gateway::{
    GatewayCommandTarget, GatewayTransition, NewGatewayCommand, ReceiveGatewayCommand,
};
use crate::domain::model::ApiEnvelope;
use crate::domain::project::project_name;
use crate::permissions::{create_private_file, prepare_private_dir, prepare_private_file};
use crate::store::{
    ClaimedImageUpload, Database, ImageUploadRecord, LATEST_SCHEMA_VERSION, NewImageUpload,
    StageImageUpload,
};
use crate::tailscale::{ServeAccess, ensure_serve};
use crate::writer::WriterHandle;
pub use auth::generate_pairing_url;
use auth::{redeem_pair_code, verify_session};
use cursor::*;
use handlers::parse_byte_range;
use middleware::{constant_time_eq, cookie_value};
use query::{parse_json, parse_optional_json, search_expression};
use router::settings_snapshot;
use stream::{StreamFilters, WsSubscribe};

#[derive(RustEmbed)]
#[folder = "web/dist"]
struct WebAssets;

static ACTIVE_CONSUMERS: AtomicU64 = AtomicU64::new(0);
static TOTAL_CONSUMERS: AtomicU64 = AtomicU64::new(0);
static SLOW_CONSUMER_DROPS: AtomicU64 = AtomicU64::new(0);

struct ConsumerGuard;
impl ConsumerGuard {
    fn new() -> Self {
        ACTIVE_CONSUMERS.fetch_add(1, Ordering::Relaxed);
        TOTAL_CONSUMERS.fetch_add(1, Ordering::Relaxed);
        Self
    }
}
impl Drop for ConsumerGuard {
    fn drop(&mut self) {
        ACTIVE_CONSUMERS.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct ApiState {
    database: Arc<Database>,
    token: Arc<String>,
    fingerprint_key: [u8; 32],
    strict_origin: bool,
    allowed_origins: Arc<Vec<String>>,
    live_modes: Arc<Vec<String>>,
    blob_downloads: Arc<Semaphore>,
    writer: WriterHandle,
    controller: ControllerRegistry,
    settings: Arc<Value>,
    tailscale: Option<Arc<ServeAccess>>,
}

#[derive(Debug, Clone)]
struct AuditPrincipal(String);

pub async fn serve(
    config: Config,
    database: Arc<Database>,
    writer: WriterHandle,
    controller: ControllerRegistry,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let controller_enabled = config.controller.enabled;
    let settings_snapshot = settings_snapshot(&config);
    let listener = TcpListener::bind(config.server.bind).await?;
    let bound_address = listener.local_addr()?;
    let tailscale = if config.server.tailscale_serve.enabled {
        Some(Arc::new(ensure_serve(
            bound_address,
            config.server.tailscale_serve.https_port,
        )?))
    } else {
        None
    };
    let mut allowed_origins = config.server.allowed_origins.clone();
    if let Some(access) = &tailscale
        && !allowed_origins
            .iter()
            .any(|origin| origin == &access.origin)
    {
        allowed_origins.push(access.origin.clone());
    }
    let token = rotate_token(&config.server.bearer_token_file)?;
    let fingerprint_key = load_or_create_key(&config.storage.fingerprint_key_file)?;
    let viewer_url = generate_pairing_url(bound_address, &token)?;
    let state = ApiState {
        database,
        token: Arc::new(token),
        fingerprint_key,
        strict_origin: config.server.strict_origin,
        allowed_origins: Arc::new(allowed_origins),
        live_modes: Arc::new(
            config
                .sources
                .iter()
                .filter(|source| source.live_mode != "off")
                .map(|source| source.live_mode.clone())
                .collect(),
        ),
        blob_downloads: Arc::new(Semaphore::new(4)),
        writer,
        controller,
        settings: Arc::new(settings_snapshot),
        tailscale: tailscale.clone(),
    };
    let protected = Router::new()
        .route("/health", get(health))
        .route("/sources", get(sources))
        .route("/projects", get(projects))
        .route("/threads", get(threads))
        .route("/threads/{thread_key}", get(thread_detail))
        .route("/threads/{thread_key}/turns", get(turns))
        .route("/threads/{thread_key}/items", get(items))
        .route("/threads/{thread_key}/events", get(thread_events))
        .route("/events", get(events))
        .route("/blobs/{blob_id}", get(blob))
        .route("/search", get(search))
        .route("/meta/capabilities", get(capabilities))
        .route("/meta/settings", get(settings))
        .route("/stream", get(sse_stream))
        .route("/stream/ws", get(ws_stream))
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            authorize,
        ));
    let mut v2 = Router::new()
        .route("/control/sources", get(controller_sources))
        .route("/control/catalog", get(controller_catalog))
        .route("/stream", get(v2_stream))
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            authorize,
        ));
    if controller_enabled {
        v2 = v2.merge(
            Router::new()
                .route(
                    "/commands",
                    get(list_gateway_commands).post(create_gateway_command),
                )
                .route("/commands/{command_id}", get(get_gateway_command))
                .route("/threads", post(create_gateway_thread))
                .route("/threads/{thread_key}/inputs", post(create_thread_input))
                .route(
                    "/requests/{request_key}/actions",
                    post(create_pending_request_action),
                )
                .route_layer(DefaultBodyLimit::max(256 * 1024))
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    authorize,
                )),
        );
        v2 = v2.merge(
            Router::new()
                .route("/uploads/images", post(upload_image))
                .route_layer(DefaultBodyLimit::max(20 * 1024 * 1024))
                .route_layer(axum_middleware::from_fn_with_state(
                    state.clone(),
                    authorize,
                )),
        );
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(web_asset))
        .route("/v1/auth/pair", post(pair_auth))
        .nest("/v1", protected)
        .nest("/v2", v2)
        .with_state(state)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(CatchPanicLayer::new())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .layer(axum_middleware::from_fn(security_headers));

    println!("Local Viewer: {viewer_url}");
    if let Some(access) = &tailscale {
        println!("Tailscale Viewer: {}", access.viewer_url);
    }
    tracing::info!(address = %bound_address, tailscale = tailscale.is_some(), token_file = %config.server.bearer_token_file.display(), "Observer Web Viewer ready");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        while !*shutdown.borrow() {
            if shutdown.changed().await.is_err() {
                break;
            }
        }
    })
    .await?;
    Ok(())
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

async fn upload_image(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !mutation_origin_allowed(&state, &headers) {
        return v2_error(
            StatusCode::FORBIDDEN,
            "ORIGIN_REJECTED",
            "mutation requests require an allowed Origin",
            None,
        );
    }
    let Some(idempotency_key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| (8..=200).contains(&value.len()) && value.is_ascii())
    else {
        return v2_error(
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_REQUIRED",
            "Idempotency-Key must contain 8 to 200 ASCII characters",
            None,
        );
    };
    if body.is_empty() || body.len() > 20 * 1024 * 1024 {
        return v2_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "IMAGE_TOO_LARGE",
            "image must contain 1 byte to 20 MiB",
            None,
        );
    }
    let Some(detected_mime) = image_mime(&body) else {
        return v2_error(
            StatusCode::BAD_REQUEST,
            "IMAGE_INVALID",
            "only PNG, JPEG, WebP, and GIF images are accepted",
            None,
        );
    };
    let declared_mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    if declared_mime != Some(detected_mime) {
        return v2_error(
            StatusCode::BAD_REQUEST,
            "IMAGE_INVALID",
            "Content-Type does not match the image signature",
            None,
        );
    }
    let key = state.fingerprint_key;
    let upload_id = blake3::keyed_hash(
        &key,
        format!("image-upload\0{}\0{idempotency_key}", principal.0).as_bytes(),
    )
    .to_hex()
    .to_string();
    let fingerprint = blake3::keyed_hash(&key, &body).to_hex().to_string();
    let relative_path = format!("{upload_id}.bin");
    let principal_id = principal.0;
    let existing = match state.database.image_upload(&upload_id) {
        Ok(existing) => existing,
        Err(error) => return v2_internal_error(error),
    };
    if let Some(existing) = existing {
        if !existing_image_matches(
            &existing,
            &principal_id,
            detected_mime,
            body.len() as i64,
            &fingerprint,
            &relative_path,
        ) {
            return image_idempotency_conflict();
        }
        if matches!(existing.state.as_str(), "staged" | "attached")
            && verified_image_path(&state, &existing).is_err()
        {
            return staged_image_integrity_error();
        }
        return image_upload_response(&existing);
    }
    let directory = state.database.image_staging_dir();
    if let Err(error) = prepare_private_dir(&directory, "image staging") {
        return v2_internal_error(error);
    }
    let path = directory.join(&relative_path);
    let mut created = false;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            match create_private_file(&path, "staged image")
                .and_then(|mut file| file.write_all(&body).map_err(Into::into))
            {
                Ok(()) => created = true,
                Err(error) => return v2_internal_error(error),
            }
        }
        Err(error) => return v2_internal_error(error.into()),
        Ok(_) => {
            let orphan = ImageUploadRecord {
                upload_id: upload_id.clone(),
                principal_id: principal_id.clone(),
                mime_type: detected_mime.into(),
                size_bytes: body.len() as i64,
                keyed_fingerprint: fingerprint.clone(),
                relative_path: relative_path.clone(),
                state: "staged".into(),
                expires_at_ms: 0,
            };
            if verified_image_path(&state, &orphan).is_err() {
                return staged_image_integrity_error();
            }
        }
    }
    let expires_at_ms = crate::clock::now_ms() + 24 * 60 * 60 * 1000;
    let staged = state.writer.stage_image_upload(NewImageUpload {
        upload_id: upload_id.clone(),
        principal_id: principal_id.clone(),
        mime_type: detected_mime.into(),
        size_bytes: body.len() as i64,
        keyed_fingerprint: fingerprint.clone(),
        relative_path: relative_path.clone(),
        expires_at_ms,
    });
    match staged {
        Ok(StageImageUpload::Created(record)) => image_upload_response(&record),
        Ok(StageImageUpload::Existing(record)) => {
            if !existing_image_matches(
                &record,
                &principal_id,
                detected_mime,
                body.len() as i64,
                &fingerprint,
                &relative_path,
            ) || (matches!(record.state.as_str(), "staged" | "attached")
                && verified_image_path(&state, &record).is_err())
            {
                if created {
                    let _ = fs::remove_file(&path);
                }
                return staged_image_integrity_error();
            }
            image_upload_response(&record)
        }
        Ok(StageImageUpload::Conflict) => {
            if created {
                let _ = fs::remove_file(&path);
            }
            image_idempotency_conflict()
        }
        Err(error) => {
            if created {
                let _ = fs::remove_file(&path);
            }
            v2_internal_error(error)
        }
    }
}

fn existing_image_matches(
    existing: &ImageUploadRecord,
    principal_id: &str,
    mime_type: &str,
    size_bytes: i64,
    keyed_fingerprint: &str,
    relative_path: &str,
) -> bool {
    existing.principal_id == principal_id
        && existing.mime_type == mime_type
        && existing.size_bytes == size_bytes
        && existing.keyed_fingerprint == keyed_fingerprint
        && existing.relative_path == relative_path
}

fn verified_image_path(state: &ApiState, upload: &ImageUploadRecord) -> Result<std::path::PathBuf> {
    let expected_relative = format!("{}.bin", upload.upload_id);
    if upload.relative_path != expected_relative {
        anyhow::bail!("staged image relative path is invalid");
    }
    let path = state
        .database
        .image_staging_dir()
        .join(&upload.relative_path);
    prepare_private_file(&path, "staged image")?;
    let bytes = fs::read(&path)?;
    if bytes.len() as i64 != upload.size_bytes
        || blake3::keyed_hash(&state.fingerprint_key, &bytes)
            .to_hex()
            .to_string()
            != upload.keyed_fingerprint
    {
        anyhow::bail!("staged image failed integrity validation");
    }
    Ok(path)
}

fn image_upload_response(upload: &ImageUploadRecord) -> Response {
    Json(json!({"apiVersion":"v2","data":{
        "uploadId":upload.upload_id,
        "mimeType":upload.mime_type,
        "sizeBytes":upload.size_bytes,
        "expiresAtMs":upload.expires_at_ms
    }}))
    .into_response()
}

fn image_idempotency_conflict() -> Response {
    v2_error(
        StatusCode::CONFLICT,
        "IDEMPOTENCY_CONFLICT",
        "Idempotency-Key is already bound to another image",
        None,
    )
}

fn staged_image_integrity_error() -> Response {
    v2_error(
        StatusCode::CONFLICT,
        "IMAGE_INVALID",
        "staged image is unavailable or failed integrity validation",
        None,
    )
}

async fn controller_sources(State(state): State<ApiState>) -> Response {
    let sources = state.controller.resolved_snapshots().await;
    Json(json!({
        "apiVersion":"v2",
        "data":sources,
        "controllerEnabled":state.settings["controller"]["enabled"]
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControllerCatalogQuery {
    source_id: Option<String>,
    thread_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct V2StreamQuery {
    cursor: Option<String>,
    thread_key: Option<String>,
}

async fn v2_stream(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<V2StreamQuery>,
) -> Response {
    let encoded_cursor = query.cursor.as_deref().or_else(|| {
        headers
            .get("last-event-id")
            .and_then(|value| value.to_str().ok())
    });
    let cursor_was_supplied = encoded_cursor.is_some();
    let mut cursor = match encoded_cursor {
        Some(value) => match decode_v2_stream_cursor(value, &state.token) {
            Ok(cursor) => cursor,
            Err(_) => {
                return v2_error(
                    StatusCode::BAD_REQUEST,
                    "CURSOR_INVALID",
                    "V2 stream cursor is invalid",
                    None,
                );
            }
        },
        None => V2StreamCursor::default(),
    };
    match state.database.retention_low_watermark() {
        Ok(low_watermark) if cursor_was_supplied && cursor.event_seq < low_watermark => {
            return v2_error(
                StatusCode::GONE,
                "CURSOR_EXPIRED",
                "event cursor is older than the retention window",
                None,
            );
        }
        Ok(low_watermark) => cursor.event_seq = cursor.event_seq.max(low_watermark),
        Err(error) => return v2_internal_error(error),
    }
    let database = state.database.clone();
    let token = state.token.clone();
    let thread_key = query.thread_key;
    let mut committed = state.writer.subscribe();
    let stream = async_stream::stream! {
        loop {
            let events = database.query_json(
                "SELECT event_seq,event_id,source_id,epoch_id,source_seq,observed_at_ms,event_at_ms,thread_key,codex_thread_id,
                  turn_id,item_id,method,phase,durability,raw_json,redaction_json,decode_status,decode_error,stored_raw_hash,blob_id
                  FROM raw_events WHERE event_seq>?1 ORDER BY event_seq LIMIT 200",
                &[&cursor.event_seq], event_row,
            ).unwrap_or_default();
            let transitions = database.query_json(
                "SELECT t.transition_seq,t.command_id,g.thread_key,g.source_id,g.source_epoch,t.from_state,
                   t.to_state,t.occurred_at_ms,t.reason_code
                 FROM command_transitions t JOIN gateway_commands g ON g.command_id=t.command_id
                 WHERE t.transition_seq>?1 ORDER BY t.transition_seq LIMIT 200",
                &[&cursor.command_transition_seq], |row| Ok(json!({
                    "kind":"commandTransition","transitionSeq":row.get::<_,i64>(0)?,
                    "commandId":row.get::<_,String>(1)?,"threadKey":row.get::<_,Option<String>>(2)?,
                    "sourceId":row.get::<_,String>(3)?,"sourceEpoch":row.get::<_,String>(4)?,
                    "fromState":row.get::<_,Option<String>>(5)?,"toState":row.get::<_,String>(6)?,
                    "occurredAtMs":row.get::<_,i64>(7)?,"reasonCode":row.get::<_,Option<String>>(8)?
                })),
            ).unwrap_or_default();
            let empty = events.is_empty() && transitions.is_empty();
            for mut row in events {
                cursor.event_seq = row["eventSeq"].as_i64().unwrap_or(cursor.event_seq);
                let matches = thread_key.as_deref().is_none_or(|expected| row["threadKey"] == expected);
                if matches {
                    row["kind"] = Value::String("observerEvent".into());
                    let id = encode_v2_stream_cursor(&cursor, &token).unwrap_or_default();
                    yield Ok::<Event, Infallible>(Event::default().id(id).event("observer_event").json_data(row).unwrap());
                }
            }
            for row in transitions {
                cursor.command_transition_seq = row["transitionSeq"].as_i64().unwrap_or(cursor.command_transition_seq);
                let matches = thread_key.as_deref().is_none_or(|expected| row["threadKey"] == expected);
                if matches {
                    let id = encode_v2_stream_cursor(&cursor, &token).unwrap_or_default();
                    yield Ok::<Event, Infallible>(Event::default().id(id).event("command_transition").json_data(row).unwrap());
                }
            }
            if empty {
                tokio::select! {
                    _ = committed.recv() => {},
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                }
            }
        }
    };
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("heartbeat"),
        )
        .into_response()
}

async fn controller_catalog(
    State(state): State<ApiState>,
    Query(query): Query<ControllerCatalogQuery>,
) -> Response {
    let Some(source_id) = query.source_id.filter(|value| !value.is_empty()) else {
        return v2_error(
            StatusCode::BAD_REQUEST,
            "QUERY_INVALID",
            "sourceId is required",
            None,
        );
    };
    let thread_id = match query.thread_key.as_deref() {
        None => None,
        Some(thread_key) => match state.database.connect().and_then(|connection| {
            connection
                .query_row(
                    "SELECT codex_thread_id FROM threads WHERE thread_key=?1",
                    [thread_key],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(Into::into)
        }) {
            Ok(Some(thread_id)) => Some(thread_id),
            Ok(None) => {
                return v2_error(
                    StatusCode::NOT_FOUND,
                    "THREAD_NOT_FOUND",
                    "the selected Thread was not found",
                    None,
                );
            }
            Err(error) => return v2_internal_error(error),
        },
    };
    match state
        .controller
        .catalog(&source_id, thread_id, query.thread_key)
        .await
    {
        Ok(catalog) => Json(json!({"apiVersion":"v2","data":catalog})).into_response(),
        Err(RegistryError::SourceNotLive | RegistryError::SourceEpochStale) => v2_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "SOURCE_NOT_LIVE",
            "the selected source is not ready",
            None,
        ),
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateGatewayCommandRequest {
    capability: String,
    target: CreateGatewayCommandTarget,
    input: Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateGatewayCommandTarget {
    source_id: String,
    source_epoch: String,
    thread_key: Option<String>,
    codex_thread_id: Option<String>,
    expected_turn_id: Option<String>,
    expected_request_id: Option<String>,
    expected_request_version: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ThreadStartCommandInput {
    cwd: String,
    model: Option<String>,
    personality: Option<String>,
    permissions: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TextCommandInput {
    text: String,
    client_user_message_id: String,
    #[serde(default)]
    upload_ids: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ThreadForkCommandInput {
    last_turn_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyCommandInput {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingCommandInput {
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThreadNameCommandInput {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewCommandInput {
    target: ReviewTargetInput,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum ReviewTargetInput {
    UncommittedChanges,
    BaseBranch { branch: String },
    Commit { sha: String, title: Option<String> },
    Custom { instructions: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GoalSetCommandInput {
    objective: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlanCommandInput {
    prompt: Option<String>,
    client_user_message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum PendingRequestActionInput {
    Approval {
        decision: String,
    },
    Permissions {
        grant: bool,
        scope: String,
        strict_auto_review: Option<bool>,
    },
    UserInput {
        answers: std::collections::BTreeMap<String, Vec<String>>,
    },
    McpElicitation {
        action: String,
        content: Option<Value>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PendingRequestActionRequest {
    source_epoch: String,
    expected_request_version: i64,
    action: PendingRequestActionInput,
}

#[derive(Debug)]
struct CommandValidationError {
    code: &'static str,
    message: &'static str,
    status: StatusCode,
}

impl std::fmt::Display for CommandValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for CommandValidationError {}

async fn create_gateway_command(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    headers: HeaderMap,
    Json(request): Json<CreateGatewayCommandRequest>,
) -> Response {
    create_gateway_command_core(&state, principal, &headers, request).await
}

async fn create_gateway_command_core(
    state: &ApiState,
    principal: AuditPrincipal,
    headers: &HeaderMap,
    request: CreateGatewayCommandRequest,
) -> Response {
    if !mutation_origin_allowed(state, headers) {
        return v2_error(
            StatusCode::FORBIDDEN,
            "ORIGIN_REJECTED",
            "mutation requests require an allowed Origin",
            None,
        );
    }
    let Some(idempotency_key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| (8..=200).contains(&value.len()) && value.is_ascii())
        .map(str::to_string)
    else {
        return v2_error(
            StatusCode::BAD_REQUEST,
            "IDEMPOTENCY_KEY_REQUIRED",
            "Idempotency-Key must contain 8 to 200 ASCII characters",
            None,
        );
    };
    if request.capability.is_empty()
        || request.capability.len() > 100
        || request.target.source_id.is_empty()
        || request.target.source_epoch.is_empty()
    {
        return v2_error(
            StatusCode::BAD_REQUEST,
            "COMMAND_INVALID",
            "capability, sourceId, and sourceEpoch are required",
            None,
        );
    }

    let canonical = match serde_json::to_vec(&request) {
        Ok(canonical) => canonical,
        Err(error) => return v2_internal_error(error.into()),
    };
    let payload_hash = blake3::hash(&canonical).to_hex().to_string();
    let input_bytes = serde_json::to_vec(&request.input)
        .map(|input| input.len())
        .unwrap_or(0);
    let command_id = uuid::Uuid::now_v7().to_string();
    let principal_id = principal.0;
    let received = state.writer.receive_gateway_command(NewGatewayCommand {
        command_id: command_id.clone(),
        principal_id: principal_id.clone(),
        capability: request.capability.clone(),
        idempotency_key,
        payload_hash,
        target: GatewayCommandTarget {
            source_id: request.target.source_id.clone(),
            source_epoch: request.target.source_epoch.clone(),
            thread_key: request.target.thread_key.clone(),
            codex_thread_id: request.target.codex_thread_id.clone(),
            expected_turn_id: request.target.expected_turn_id.clone(),
            expected_request_id: request.target.expected_request_id.clone(),
            expected_request_version: request.target.expected_request_version,
        },
        input_summary_json: json!({"jsonBytes":input_bytes}).to_string(),
    });
    let command = match received {
        Ok(ReceiveGatewayCommand::Created(command)) => command,
        Ok(ReceiveGatewayCommand::Existing(command)) => {
            return gateway_command_response(StatusCode::OK, command);
        }
        Ok(ReceiveGatewayCommand::Conflict) => {
            return v2_error(
                StatusCode::CONFLICT,
                "IDEMPOTENCY_CONFLICT",
                "Idempotency-Key is already bound to a different payload",
                None,
            );
        }
        Err(error) => return v2_internal_error(error),
    };

    let upload_ids = if matches!(request.capability.as_str(), "turn.start" | "turn.steer") {
        match request.input.get("uploadIds") {
            None => Vec::new(),
            Some(Value::Array(values)) if values.iter().all(|value| value.as_str().is_some()) => {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            }
            Some(_) => {
                return reject_gateway_command(
                    state,
                    &command.command_id,
                    "COMMAND_INVALID",
                    "uploadIds must be an array of strings",
                    StatusCode::BAD_REQUEST,
                );
            }
        }
    } else {
        Vec::new()
    };
    let claimed_images =
        match state
            .writer
            .claim_image_uploads(&command.command_id, &principal_id, upload_ids)
        {
            Ok(paths) => paths,
            Err(_) => {
                return reject_gateway_command(
                    state,
                    &command.command_id,
                    "IMAGE_INVALID",
                    "one or more staged images are unavailable",
                    StatusCode::BAD_REQUEST,
                );
            }
        };
    let image_paths = match claimed_images
        .iter()
        .map(|upload| verified_claimed_image_path(state, upload))
        .collect::<Result<Vec<_>>>()
    {
        Ok(paths) => paths,
        Err(_) => {
            cleanup_command_image_files(state, &command.command_id);
            return reject_gateway_command(
                state,
                &command.command_id,
                "IMAGE_INVALID",
                "one or more staged images failed integrity validation",
                StatusCode::CONFLICT,
            );
        }
    };
    let operation = match controller_operation(&request, image_paths) {
        Ok(operation) => operation,
        Err(error) => {
            cleanup_command_image_files(state, &command.command_id);
            return reject_gateway_command(
                state,
                &command.command_id,
                error.code,
                error.message,
                error.status,
            );
        }
    };
    if let Err(error) = state.writer.transition_gateway_command(gateway_transition(
        &command.command_id,
        "authorized",
        "allow",
        "authorized",
    )) {
        cleanup_command_image_files(state, &command.command_id);
        return v2_internal_error(error);
    }
    match state
        .controller
        .dispatch(
            &command.target.source_id,
            &command.target.source_epoch,
            ActorCommand {
                command_id: command.command_id.clone(),
                operation,
            },
        )
        .await
    {
        Err(RegistryError::SourceNotLive) => {
            cleanup_command_image_files(state, &command.command_id);
            reject_gateway_command(
                state,
                &command.command_id,
                "SOURCE_NOT_LIVE",
                "the selected source is not ready",
                StatusCode::SERVICE_UNAVAILABLE,
            )
        }
        Err(RegistryError::SourceEpochStale) => {
            cleanup_command_image_files(state, &command.command_id);
            reject_gateway_command(
                state,
                &command.command_id,
                "SOURCE_EPOCH_STALE",
                "the selected source epoch is stale",
                StatusCode::CONFLICT,
            )
        }
        Ok(record) => {
            if matches!(record.state.as_str(), "rejected" | "failed" | "cancelled") {
                cleanup_command_image_files(state, &command.command_id);
            }
            gateway_dispatch_response(record)
        }
    }
}

fn cleanup_command_image_files(state: &ApiState, command_id: &str) {
    if let Ok(paths) = state.writer.cleanup_command_images(command_id) {
        for path in paths {
            let _ = fs::remove_file(path);
        }
    }
}

fn verified_claimed_image_path(state: &ApiState, upload: &ClaimedImageUpload) -> Result<String> {
    let expected_relative = format!("{}.bin", upload.upload_id);
    if upload.relative_path != expected_relative {
        anyhow::bail!("staged image relative path is invalid");
    }
    let path = state
        .database
        .image_staging_dir()
        .join(&upload.relative_path);
    prepare_private_file(&path, "staged image")?;
    let bytes = fs::read(&path)?;
    if blake3::keyed_hash(&state.fingerprint_key, &bytes)
        .to_hex()
        .to_string()
        != upload.keyed_fingerprint
    {
        anyhow::bail!("staged image failed integrity validation");
    }
    Ok(path.to_string_lossy().to_string())
}

fn controller_operation(
    request: &CreateGatewayCommandRequest,
    image_paths: Vec<String>,
) -> std::result::Result<ControllerOperation, CommandValidationError> {
    let target_thread = || {
        let Some(thread_id) = request
            .target
            .codex_thread_id
            .as_deref()
            .filter(|value| !value.is_empty() && value.len() <= 200)
        else {
            return Err(command_invalid(
                "codexThreadId is required for this capability",
            ));
        };
        let Some(thread_key) = request
            .target
            .thread_key
            .as_deref()
            .filter(|value| !value.is_empty() && value.len() <= 200)
        else {
            return Err(command_invalid("threadKey is required for this capability"));
        };
        Ok((thread_id.to_string(), thread_key.to_string()))
    };
    match request.capability.as_str() {
        "thread.start" => {
            let input: ThreadStartCommandInput = decode_command_input(&request.input)?;
            let cwd_path = std::path::Path::new(&input.cwd);
            if !cwd_path.is_absolute() {
                return Err(command_invalid("cwd must be an absolute directory"));
            }
            let canonical = fs::canonicalize(cwd_path)
                .ok()
                .filter(|path| path.is_dir())
                .and_then(|path| path.to_str().map(str::to_string))
                .ok_or_else(|| command_invalid("cwd must be an existing local directory"))?;
            if input
                .model
                .as_ref()
                .is_some_and(|value| value.is_empty() || value.len() > 200)
                || input
                    .permissions
                    .as_ref()
                    .is_some_and(|value| value.is_empty() || value.len() > 200)
                || input
                    .personality
                    .as_deref()
                    .is_some_and(|value| !matches!(value, "none" | "friendly" | "pragmatic"))
            {
                return Err(command_invalid(
                    "model, personality, or permissions is invalid",
                ));
            }
            Ok(ControllerOperation::ThreadStart {
                cwd: canonical,
                model: input.model,
                personality: input.personality,
                permissions: input.permissions,
            })
        }
        "thread.resume" => {
            let _: EmptyCommandInput = decode_command_input(&request.input)?;
            let (thread_id, thread_key) = target_thread()?;
            Ok(ControllerOperation::ThreadResume {
                thread_id,
                thread_key,
            })
        }
        "thread.fork" => {
            let input: ThreadForkCommandInput = decode_command_input(&request.input)?;
            if input
                .last_turn_id
                .as_ref()
                .is_some_and(|value| value.is_empty() || value.len() > 200)
            {
                return Err(command_invalid("lastTurnId is invalid"));
            }
            let (thread_id, thread_key) = target_thread()?;
            Ok(ControllerOperation::ThreadFork {
                thread_id,
                thread_key,
                last_turn_id: input.last_turn_id,
            })
        }
        "turn.start" | "turn.steer" => {
            let input: TextCommandInput = decode_command_input(&request.input)?;
            if input.upload_ids.len() > 4
                || input
                    .upload_ids
                    .iter()
                    .any(|value| value.is_empty() || value.len() > 200)
                || input
                    .upload_ids
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != input.upload_ids.len()
                || image_paths.len() != input.upload_ids.len()
            {
                return Err(command_invalid("uploadIds are invalid"));
            }
            let text = if input.text.is_empty() && !image_paths.is_empty() {
                String::new()
            } else {
                normalize_user_text(&input.text)?
            };
            if input.client_user_message_id.is_empty() || input.client_user_message_id.len() > 200 {
                return Err(command_invalid("clientUserMessageId is invalid"));
            }
            let (thread_id, thread_key) = target_thread()?;
            if request.capability == "turn.start" {
                if request.target.expected_turn_id.is_some() {
                    return Err(command_invalid("turn.start does not accept expectedTurnId"));
                }
                Ok(ControllerOperation::TurnStart {
                    thread_id,
                    thread_key,
                    client_user_message_id: input.client_user_message_id,
                    text,
                    image_paths,
                })
            } else {
                let expected_turn_id = expected_turn_id(request)?;
                Ok(ControllerOperation::TurnSteer {
                    thread_id,
                    thread_key,
                    expected_turn_id,
                    client_user_message_id: input.client_user_message_id,
                    text,
                    image_paths,
                })
            }
        }
        "turn.interrupt" => {
            let _: EmptyCommandInput = decode_command_input(&request.input)?;
            let (thread_id, thread_key) = target_thread()?;
            Ok(ControllerOperation::TurnInterrupt {
                thread_id,
                thread_key,
                expected_turn_id: expected_turn_id(request)?,
            })
        }
        "thread.settings.model"
        | "thread.settings.reasoning"
        | "thread.settings.personality"
        | "thread.settings.permissions" => {
            let input: SettingCommandInput = decode_command_input(&request.input)?;
            if input.value.is_empty() || input.value.len() > 200 {
                return Err(command_invalid("setting value is invalid"));
            }
            let (thread_id, thread_key) = target_thread()?;
            let setting = match request.capability.as_str() {
                "thread.settings.model" => ThreadSetting::Model(input.value),
                "thread.settings.reasoning" => ThreadSetting::ReasoningEffort(input.value),
                "thread.settings.personality" => {
                    if !matches!(input.value.as_str(), "none" | "friendly" | "pragmatic") {
                        return Err(command_invalid("personality is invalid"));
                    }
                    ThreadSetting::Personality(input.value)
                }
                "thread.settings.permissions" => ThreadSetting::Permissions(input.value),
                _ => unreachable!(),
            };
            Ok(ControllerOperation::ThreadSettingsUpdate {
                thread_id,
                thread_key,
                setting,
            })
        }
        "thread.plan" => {
            let input: PlanCommandInput = decode_command_input(&request.input)?;
            let prompt = input
                .prompt
                .map(|value| normalize_plan_prompt(&value))
                .transpose()?;
            let client_user_message_id = input
                .client_user_message_id
                .filter(|value| !value.is_empty() && value.len() <= 200);
            if prompt.is_some() != client_user_message_id.is_some() {
                return Err(command_invalid(
                    "Plan prompt and clientUserMessageId must be supplied together",
                ));
            }
            let (thread_id, thread_key) = target_thread()?;
            Ok(ControllerOperation::Plan {
                thread_id,
                thread_key,
                expected_turn_id: request.target.expected_turn_id.clone(),
                client_user_message_id,
                prompt,
            })
        }
        "thread.name.set" => {
            let input: ThreadNameCommandInput = decode_command_input(&request.input)?;
            if input.name.trim().is_empty() || input.name.len() > 200 {
                return Err(command_invalid("Thread name is invalid"));
            }
            let (thread_id, thread_key) = target_thread()?;
            Ok(ControllerOperation::ThreadNameSet {
                thread_id,
                thread_key,
                name: input.name,
            })
        }
        "thread.archive" | "thread.compact" | "thread.goal.get" | "thread.goal.clear" => {
            let _: EmptyCommandInput = decode_command_input(&request.input)?;
            let (thread_id, thread_key) = target_thread()?;
            match request.capability.as_str() {
                "thread.archive" => Ok(ControllerOperation::ThreadArchive {
                    thread_id,
                    thread_key,
                }),
                "thread.compact" => Ok(ControllerOperation::ThreadCompact {
                    thread_id,
                    thread_key,
                }),
                "thread.goal.get" => Ok(ControllerOperation::GoalGet {
                    thread_id,
                    thread_key,
                }),
                "thread.goal.clear" => Ok(ControllerOperation::GoalClear {
                    thread_id,
                    thread_key,
                }),
                _ => unreachable!(),
            }
        }
        "review.start" => {
            let input: ReviewCommandInput = decode_command_input(&request.input)?;
            let target = match input.target {
                ReviewTargetInput::UncommittedChanges => ReviewTarget::UncommittedChanges,
                ReviewTargetInput::BaseBranch { branch }
                    if !branch.is_empty() && branch.len() <= 200 =>
                {
                    ReviewTarget::BaseBranch(branch)
                }
                ReviewTargetInput::Commit { sha, title }
                    if !sha.is_empty()
                        && sha.len() <= 200
                        && title.as_ref().is_none_or(|value| value.len() <= 200) =>
                {
                    ReviewTarget::Commit { sha, title }
                }
                ReviewTargetInput::Custom { instructions }
                    if !instructions.is_empty() && instructions.len() <= 20_000 =>
                {
                    ReviewTarget::Custom(instructions)
                }
                _ => return Err(command_invalid("review target is invalid")),
            };
            let (thread_id, thread_key) = target_thread()?;
            Ok(ControllerOperation::ReviewStart {
                thread_id,
                thread_key,
                target,
            })
        }
        "thread.goal.set" => {
            let input: GoalSetCommandInput = decode_command_input(&request.input)?;
            if input
                .objective
                .as_ref()
                .is_some_and(|value| value.trim().is_empty() || value.len() > 100_000)
                || input
                    .status
                    .as_deref()
                    .is_some_and(|value| !matches!(value, "active" | "paused"))
                || (input.objective.is_none() && input.status.is_none())
            {
                return Err(command_invalid("goal update is invalid"));
            }
            let (thread_id, thread_key) = target_thread()?;
            Ok(ControllerOperation::GoalSet {
                thread_id,
                thread_key,
                objective: input.objective,
                status: input.status,
            })
        }
        "request.action" => {
            let input: PendingRequestActionInput = decode_command_input(&request.input)?;
            let (thread_id, thread_key) = target_thread()?;
            let request_id = request
                .target
                .expected_request_id
                .as_deref()
                .filter(|value| !value.is_empty() && value.len() <= 200)
                .ok_or_else(|| {
                    command_invalid("expectedRequestId is required for this capability")
                })?
                .to_string();
            let expected_request_version = request
                .target
                .expected_request_version
                .filter(|version| *version > 0)
                .ok_or_else(|| {
                    command_invalid("expectedRequestVersion is required for this capability")
                })?;
            let action = match input {
                PendingRequestActionInput::Approval { decision } => {
                    PendingRequestAction::Approval { decision }
                }
                PendingRequestActionInput::Permissions {
                    grant,
                    scope,
                    strict_auto_review,
                } => PendingRequestAction::Permissions {
                    grant,
                    scope,
                    strict_auto_review,
                },
                PendingRequestActionInput::UserInput { answers } => {
                    PendingRequestAction::UserInput { answers }
                }
                PendingRequestActionInput::McpElicitation { action, content } => {
                    PendingRequestAction::McpElicitation { action, content }
                }
            };
            Ok(ControllerOperation::PendingRequestAction {
                thread_id,
                thread_key,
                request_id,
                expected_request_version,
                action,
            })
        }
        _ => Err(CommandValidationError {
            code: "CAPABILITY_UNAVAILABLE",
            message: "the requested capability is not published by this Gateway version",
            status: StatusCode::CONFLICT,
        }),
    }
}

async fn create_pending_request_action(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    headers: HeaderMap,
    Path(request_key): Path<String>,
    Json(request): Json<PendingRequestActionRequest>,
) -> Response {
    if !mutation_origin_allowed(&state, &headers) {
        return v2_error(
            StatusCode::FORBIDDEN,
            "ORIGIN_REJECTED",
            "mutation requests require an allowed Origin",
            None,
        );
    }
    let key = match decode_request_key(&request_key, &state.token) {
        Ok(key) => key,
        Err(_) => {
            return v2_error(
                StatusCode::BAD_REQUEST,
                "REQUEST_NOT_PENDING",
                "requestKey is invalid",
                None,
            );
        }
    };
    if request.source_epoch != key.source_epoch {
        return v2_error(
            StatusCode::CONFLICT,
            "SOURCE_EPOCH_STALE",
            "sourceEpoch does not match requestKey",
            None,
        );
    }
    if request.expected_request_version <= 0 {
        return v2_error(
            StatusCode::BAD_REQUEST,
            "REQUEST_NOT_PENDING",
            "expectedRequestVersion must be positive",
            None,
        );
    }
    let target = match state.database.pending_request_target(
        &key.source_id,
        &key.source_epoch,
        &key.request_id,
    ) {
        Ok(Some(target)) => target,
        Ok(None) => {
            return v2_error(
                StatusCode::CONFLICT,
                "REQUEST_NOT_PENDING",
                "the pending request was not found",
                None,
            );
        }
        Err(error) => return v2_internal_error(error),
    };
    if target.source_id != key.source_id
        || target.source_epoch != key.source_epoch
        || target.request_id != key.request_id
    {
        return v2_error(
            StatusCode::CONFLICT,
            "REQUEST_NOT_PENDING",
            "requestKey no longer identifies this pending request",
            None,
        );
    }
    let error = match target.state.as_str() {
        "resolving" | "resolved" => Some((
            "REQUEST_ALREADY_RESOLVED",
            "another client already resolved this request",
        )),
        "source_disconnected" => Some((
            "SOURCE_EPOCH_STALE",
            "the request belongs to an inactive source epoch",
        )),
        "pending" if target.request_version != request.expected_request_version => Some((
            "REQUEST_NOT_PENDING",
            "the pending request version no longer matches",
        )),
        "pending" => None,
        _ => Some(("REQUEST_NOT_PENDING", "the request is not pending")),
    };
    if let Some((code, message)) = error {
        return v2_error(StatusCode::CONFLICT, code, message, None);
    }
    let action_matches_request = matches!(
        (&target.request_type, &request.action),
        (
            request_type,
            PendingRequestActionInput::Approval { .. }
                | PendingRequestActionInput::Permissions { .. }
        ) if request_type == "approval"
    ) || matches!(
        (&target.request_type, &request.action),
        (request_type, PendingRequestActionInput::UserInput { .. }) if request_type == "user_input"
    ) || matches!(
        (&target.request_type, &request.action),
        (request_type, PendingRequestActionInput::McpElicitation { .. }) if request_type == "mcp_elicitation"
    );
    if !action_matches_request {
        return v2_error(
            StatusCode::BAD_REQUEST,
            "COMMAND_INVALID",
            "action type does not match the pending request",
            None,
        );
    }
    let Some(thread_key) = target.thread_key else {
        return v2_error(
            StatusCode::CONFLICT,
            "THREAD_NOT_LOADED",
            "the pending request is not associated with a projected Thread",
            None,
        );
    };
    let Some(codex_thread_id) = target.codex_thread_id else {
        return v2_error(
            StatusCode::CONFLICT,
            "THREAD_NOT_LOADED",
            "the pending request Thread is not available",
            None,
        );
    };
    create_gateway_command_core(
        &state,
        principal,
        &headers,
        CreateGatewayCommandRequest {
            capability: "request.action".into(),
            target: CreateGatewayCommandTarget {
                source_id: key.source_id,
                source_epoch: key.source_epoch,
                thread_key: Some(thread_key),
                codex_thread_id: Some(codex_thread_id),
                expected_turn_id: None,
                expected_request_id: Some(key.request_id),
                expected_request_version: Some(request.expected_request_version),
            },
            input: serde_json::to_value(request.action).unwrap_or(Value::Null),
        },
    )
    .await
}

fn decode_command_input<T: for<'de> Deserialize<'de>>(
    input: &Value,
) -> std::result::Result<T, CommandValidationError> {
    serde_json::from_value(input.clone()).map_err(|_| command_invalid("command input is invalid"))
}

fn expected_turn_id(
    request: &CreateGatewayCommandRequest,
) -> std::result::Result<String, CommandValidationError> {
    request
        .target
        .expected_turn_id
        .as_deref()
        .filter(|value| !value.is_empty() && value.len() <= 200)
        .map(str::to_string)
        .ok_or_else(|| command_invalid("expectedTurnId is required for this capability"))
}

fn normalize_user_text(text: &str) -> std::result::Result<String, CommandValidationError> {
    if text.is_empty() || text.len() > 200_000 {
        return Err(command_invalid("text must contain 1 to 200000 bytes"));
    }
    if let Some(literal) = text.strip_prefix("//") {
        return Ok(format!("/{literal}"));
    }
    if text.starts_with('/') {
        return Err(CommandValidationError {
            code: "UNKNOWN_COMMAND",
            message: "the Slash command is not available",
            status: StatusCode::BAD_REQUEST,
        });
    }
    Ok(text.to_string())
}

fn normalize_plan_prompt(text: &str) -> std::result::Result<String, CommandValidationError> {
    if text.is_empty() || text.len() > 200_000 {
        return Err(command_invalid(
            "Plan prompt must contain 1 to 200000 bytes",
        ));
    }
    Ok(text.to_string())
}

fn command_invalid(message: &'static str) -> CommandValidationError {
    CommandValidationError {
        code: "COMMAND_INVALID",
        message,
        status: StatusCode::BAD_REQUEST,
    }
}

fn gateway_dispatch_response(command: crate::domain::gateway::GatewayCommandRecord) -> Response {
    if let Some(error) = command.error.as_ref() {
        let status = match error.code.as_str() {
            "THREAD_NOT_LOADED" | "TURN_STATE_CONFLICT" | "SOURCE_EPOCH_STALE" => {
                StatusCode::CONFLICT
            }
            "SOURCE_NOT_LIVE" => StatusCode::SERVICE_UNAVAILABLE,
            "UPSTREAM_REJECTED" | "OUTCOME_UNKNOWN" => StatusCode::BAD_GATEWAY,
            _ => StatusCode::CONFLICT,
        };
        return v2_error(
            status,
            &error.code,
            &error.message,
            Some(&command.command_id),
        );
    }
    let status = if matches!(command.state.as_str(), "running" | "accepted_by_source") {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    gateway_command_response(status, command)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateGatewayThreadRequest {
    source_id: String,
    source_epoch: String,
    cwd: String,
    model: Option<String>,
    personality: Option<String>,
    permissions: Option<String>,
}

async fn create_gateway_thread(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    headers: HeaderMap,
    Json(request): Json<CreateGatewayThreadRequest>,
) -> Response {
    create_gateway_command_core(
        &state,
        principal,
        &headers,
        CreateGatewayCommandRequest {
            capability: "thread.start".into(),
            target: CreateGatewayCommandTarget {
                source_id: request.source_id,
                source_epoch: request.source_epoch,
                thread_key: None,
                codex_thread_id: None,
                expected_turn_id: None,
                expected_request_id: None,
                expected_request_version: None,
            },
            input: json!({
                "cwd":request.cwd,
                "model":request.model,
                "personality":request.personality,
                "permissions":request.permissions,
            }),
        },
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateThreadInputRequest {
    source_id: String,
    source_epoch: String,
    codex_thread_id: String,
    expected_turn_id: Option<String>,
    client_user_message_id: String,
    text: String,
    #[serde(default)]
    upload_ids: Vec<String>,
}

async fn create_thread_input(
    State(state): State<ApiState>,
    Extension(principal): Extension<AuditPrincipal>,
    headers: HeaderMap,
    Path(thread_key): Path<String>,
    Json(request): Json<CreateThreadInputRequest>,
) -> Response {
    if !mutation_origin_allowed(&state, &headers) {
        return v2_error(
            StatusCode::FORBIDDEN,
            "ORIGIN_REJECTED",
            "mutation requests require an allowed Origin",
            None,
        );
    }
    let (capability, input) = if request.text.starts_with('/') && !request.text.starts_with("//") {
        let (slash, argument) = slash_parts(&request.text);
        match (slash, argument) {
            ("/new", None) => {
                return interaction_required("thread.start", "newThread");
            }
            ("/resume", None) => ("thread.resume", json!({})),
            ("/fork", None) => ("thread.fork", json!({"lastTurnId":null})),
            ("/interrupt", None) => ("turn.interrupt", json!({})),
            ("/plan", prompt) => (
                "thread.plan",
                json!({
                    "prompt":prompt,
                    "clientUserMessageId":prompt.map(|_| request.client_user_message_id.as_str()),
                }),
            ),
            ("/rename", None) => {
                return interaction_required("thread.name.set", "threadNameInput");
            }
            ("/rename", Some(name)) => ("thread.name.set", json!({"name":name})),
            ("/archive", None) => ("thread.archive", json!({})),
            ("/compact", None) => ("thread.compact", json!({})),
            ("/review", None) => (
                "review.start",
                json!({"target":{"type":"uncommittedChanges"}}),
            ),
            ("/goal", None) => ("thread.goal.get", json!({})),
            ("/goal", Some("pause")) => (
                "thread.goal.set",
                json!({"objective":null,"status":"paused"}),
            ),
            ("/goal", Some("resume")) => (
                "thread.goal.set",
                json!({"objective":null,"status":"active"}),
            ),
            ("/goal", Some("clear")) => ("thread.goal.clear", json!({})),
            ("/goal", Some(objective)) => (
                "thread.goal.set",
                json!({"objective":objective,"status":"active"}),
            ),
            ("/status", None) => {
                return local_control_card(
                    &state,
                    &request.source_id,
                    &request.source_epoch,
                    &thread_key,
                    &request.codex_thread_id,
                    "status",
                )
                .await;
            }
            ("/mcp", None | Some("verbose")) => {
                return local_control_card(
                    &state,
                    &request.source_id,
                    &request.source_epoch,
                    &thread_key,
                    &request.codex_thread_id,
                    "mcp",
                )
                .await;
            }
            ("/usage", None) => {
                return local_control_card(
                    &state,
                    &request.source_id,
                    &request.source_epoch,
                    &thread_key,
                    &request.codex_thread_id,
                    "usage",
                )
                .await;
            }
            ("/model", None) => {
                return interaction_required("thread.settings.model", "modelPicker");
            }
            ("/reasoning", None) => {
                return interaction_required("thread.settings.reasoning", "reasoningPicker");
            }
            ("/personality", None) => {
                return interaction_required("thread.settings.personality", "personalityPicker");
            }
            ("/permissions", None) => {
                return interaction_required("thread.settings.permissions", "permissionPicker");
            }
            ("/model", Some(value)) => ("thread.settings.model", json!({"value":value})),
            ("/reasoning", Some(value)) => ("thread.settings.reasoning", json!({"value":value})),
            ("/personality", Some(value)) => {
                ("thread.settings.personality", json!({"value":value}))
            }
            ("/permissions", Some(value)) => {
                ("thread.settings.permissions", json!({"value":value}))
            }
            _ => (
                "turn.start",
                json!({
                    "text":request.text,
                    "clientUserMessageId":request.client_user_message_id,
                    "uploadIds":request.upload_ids,
                }),
            ),
        }
    } else if request.expected_turn_id.is_some() {
        (
            "turn.steer",
            json!({
                "text":request.text,
                "clientUserMessageId":request.client_user_message_id,
                "uploadIds":request.upload_ids,
            }),
        )
    } else {
        (
            "turn.start",
            json!({
                "text":request.text,
                "clientUserMessageId":request.client_user_message_id,
                "uploadIds":request.upload_ids,
            }),
        )
    };
    create_gateway_command_core(
        &state,
        principal,
        &headers,
        CreateGatewayCommandRequest {
            capability: capability.into(),
            target: CreateGatewayCommandTarget {
                source_id: request.source_id,
                source_epoch: request.source_epoch,
                thread_key: Some(thread_key),
                codex_thread_id: Some(request.codex_thread_id),
                expected_turn_id: request.expected_turn_id,
                expected_request_id: None,
                expected_request_version: None,
            },
            input,
        },
    )
    .await
}

fn slash_parts(text: &str) -> (&str, Option<&str>) {
    let Some(split) = text.find(char::is_whitespace) else {
        return (text, None);
    };
    let (command, remainder) = text.split_at(split);
    let argument = remainder.trim();
    (command, (!argument.is_empty()).then_some(argument))
}

async fn local_control_card(
    state: &ApiState,
    source_id: &str,
    source_epoch: &str,
    thread_key: &str,
    thread_id: &str,
    card_type: &str,
) -> Response {
    let catalog = match state
        .controller
        .catalog(
            source_id,
            Some(thread_id.to_string()),
            Some(thread_key.to_string()),
        )
        .await
    {
        Ok(catalog) => catalog,
        Err(RegistryError::SourceNotLive | RegistryError::SourceEpochStale) => {
            return v2_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "SOURCE_NOT_LIVE",
                "the selected source is not ready",
                None,
            );
        }
    };
    if catalog.source_epoch != source_epoch {
        return v2_error(
            StatusCode::CONFLICT,
            "SOURCE_EPOCH_STALE",
            "the selected source epoch is stale",
            None,
        );
    }
    let content = match card_type {
        "status" => {
            let latest_command = state
                .database
                .gateway_commands_page(Some(thread_key), None, None, None, 1)
                .ok()
                .and_then(|page| page.commands.into_iter().next());
            json!({
                "threadLoaded":catalog.thread_loaded,
                "activeTurnId":catalog.active_turn_id,
                "collaborationMode":catalog.collaboration_mode,
                "goal":catalog.goal,
                "latestCommand":latest_command,
            })
        }
        "mcp" => {
            let Some(entry) = catalog
                .capabilities
                .entries
                .get("mcpServerStatus/list")
                .filter(|entry| entry.available)
                .and_then(|entry| entry.data.clone())
            else {
                return v2_error(
                    StatusCode::CONFLICT,
                    "CAPABILITY_UNAVAILABLE",
                    "MCP status is unavailable for this source",
                    None,
                );
            };
            entry
        }
        "usage" => {
            let usage = catalog
                .capabilities
                .entries
                .get("account/usage/read")
                .filter(|entry| entry.available)
                .and_then(|entry| entry.data.clone());
            let limits = catalog
                .capabilities
                .entries
                .get("account/rateLimits/read")
                .filter(|entry| entry.available)
                .and_then(|entry| entry.data.clone());
            if usage.is_none() || limits.is_none() {
                return v2_error(
                    StatusCode::CONFLICT,
                    "CAPABILITY_UNAVAILABLE",
                    "usage or rate-limit status is unavailable for this source",
                    None,
                );
            }
            json!({"usage":usage,"rateLimits":limits})
        }
        _ => {
            return v2_error(
                StatusCode::BAD_REQUEST,
                "COMMAND_INVALID",
                "status card type is invalid",
                None,
            );
        }
    };
    Json(json!({
        "apiVersion":"v2",
        "data":{
            "kind":"gatewayStatusCard",
            "cardType":card_type,
            "sourceId":catalog.source_id,
            "sourceEpoch":catalog.source_epoch,
            "threadKey":thread_key,
            "content":content,
        }
    }))
    .into_response()
}

fn interaction_required(capability: &str, interaction_type: &str) -> Response {
    v2_error_details(
        StatusCode::CONFLICT,
        "INTERACTION_REQUIRED",
        "the command requires a structured selection",
        json!({
            "capability":capability,
            "interaction":{"type":interaction_type}
        }),
    )
}

async fn get_gateway_command(
    State(state): State<ApiState>,
    Path(command_id): Path<String>,
) -> Response {
    match state.database.gateway_command(&command_id) {
        Ok(Some(command)) => gateway_command_response(StatusCode::OK, command),
        Ok(None) => v2_error(
            StatusCode::NOT_FOUND,
            "COMMAND_NOT_FOUND",
            "Gateway command was not found",
            None,
        ),
        Err(error) => v2_internal_error(error),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GatewayCommandQuery {
    thread_key: Option<String>,
    state: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

async fn list_gateway_commands(
    State(state): State<ApiState>,
    Query(query): Query<GatewayCommandQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return v2_error(
            StatusCode::BAD_REQUEST,
            "QUERY_INVALID",
            "limit must be between 1 and 100",
            None,
        );
    }
    if query.state.as_deref().is_some_and(|state| {
        !matches!(
            state,
            "received"
                | "authorized"
                | "dispatching"
                | "accepted_by_source"
                | "running"
                | "completed"
                | "rejected"
                | "failed"
                | "cancelled"
                | "outcome_unknown"
        )
    }) {
        return v2_error(
            StatusCode::BAD_REQUEST,
            "QUERY_INVALID",
            "state is not a Gateway command state",
            None,
        );
    }
    let fingerprint = query_fingerprint(&json!({
        "threadKey":query.thread_key,
        "state":query.state
    }));
    let cursor = match decode_bound_page_cursor(
        query.cursor.as_deref(),
        "v2.commands",
        &fingerprint,
        &state.token,
    ) {
        Ok(cursor) => cursor,
        Err(CursorFailure::Invalid(_)) => {
            return v2_error(
                StatusCode::BAD_REQUEST,
                "CURSOR_INVALID",
                "cursor is invalid for this command query",
                None,
            );
        }
        Err(CursorFailure::Internal(error)) => return v2_internal_error(error),
    };
    let page = state.database.gateway_commands_page(
        query.thread_key.as_deref(),
        query.state.as_deref(),
        cursor.as_ref().map(|cursor| cursor.as_of_event_seq),
        cursor
            .as_ref()
            .map(|cursor| (cursor.last_sort, cursor.last_key.as_str())),
        limit + 1,
    );
    let mut page = match page {
        Ok(page) => page,
        Err(error) => return v2_internal_error(error),
    };
    let has_more = page.commands.len() > limit;
    if has_more {
        page.commands.truncate(limit);
    }
    let next_cursor = if has_more {
        page.commands.last().and_then(|command| {
            encode_page_cursor(
                &PageCursor {
                    endpoint: "v2.commands".into(),
                    query_fingerprint: fingerprint,
                    as_of_event_seq: page.as_of_rowid,
                    last_sort: command.created_at_ms,
                    last_key: command.command_id.clone(),
                    last_secondary: None,
                },
                &state.token,
            )
            .ok()
        })
    } else {
        None
    };
    Json(json!({
        "apiVersion":"v2",
        "data":page.commands,
        "nextCursor":next_cursor
    }))
    .into_response()
}

fn mutation_origin_allowed(state: &ApiState, headers: &HeaderMap) -> bool {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| {
            state
                .allowed_origins
                .iter()
                .any(|allowed| allowed == origin)
        })
}

fn gateway_transition(
    command_id: &str,
    to_state: &str,
    decision: &str,
    outcome: &str,
) -> GatewayTransition {
    GatewayTransition {
        command_id: command_id.into(),
        to_state: to_state.into(),
        result_summary_json: None,
        error_code: None,
        error_message: None,
        reason_code: None,
        decision: decision.into(),
        outcome: outcome.into(),
    }
}

fn reject_gateway_command(
    state: &ApiState,
    command_id: &str,
    code: &str,
    message: &str,
    status: StatusCode,
) -> Response {
    let mut transition = gateway_transition(command_id, "rejected", "deny", "rejected");
    transition.error_code = Some(code.into());
    transition.error_message = Some(message.into());
    transition.reason_code = Some(code.into());
    match state.writer.transition_gateway_command(transition) {
        Ok(_) => v2_error(status, code, message, Some(command_id)),
        Err(error) => v2_internal_error(error),
    }
}

fn gateway_command_response(
    status: StatusCode,
    command: crate::domain::gateway::GatewayCommandRecord,
) -> Response {
    (status, Json(json!({"apiVersion":"v2","data":command}))).into_response()
}

fn v2_error(status: StatusCode, code: &str, message: &str, command_id: Option<&str>) -> Response {
    v2_error_details(status, code, message, json!({"commandId":command_id}))
}

fn v2_error_details(status: StatusCode, code: &str, message: &str, details: Value) -> Response {
    (
        status,
        Json(json!({
            "apiVersion":"v2",
            "error":{
                "code":code,
                "message":message,
                "requestId":uuid::Uuid::now_v7().to_string(),
                "details":details
            }
        })),
    )
        .into_response()
}

fn v2_internal_error(error: anyhow::Error) -> Response {
    tracing::error!(error = %error, "V2 command persistence failed");
    v2_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "INTERNAL_ERROR",
        "internal command error",
        None,
    )
}

async fn not_found() -> Response {
    api_error(
        StatusCode::NOT_FOUND,
        "ROUTE_NOT_FOUND",
        "route was not found",
    )
}

async fn method_not_allowed() -> Response {
    api_error(
        StatusCode::METHOD_NOT_ALLOWED,
        "METHOD_NOT_ALLOWED",
        "method is not allowed",
    )
}

async fn authorize(State(state): State<ApiState>, mut request: Request, next: Next) -> Response {
    let bearer_authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token.as_bytes(), state.token.as_bytes()));
    let cookie_authorized = request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| cookie_value(cookies, "observer_session"))
        .is_some_and(|cookie| verify_session(&state.token, cookie, chrono::Utc::now().timestamp()));
    let tailscale_principal = state
        .tailscale
        .as_ref()
        .and_then(|access| tailscale_request_principal(access, &request));
    let principal = if bearer_authorized {
        Some("local_bearer".to_string())
    } else if cookie_authorized {
        Some("local_cookie".to_string())
    } else {
        tailscale_principal
    };
    let Some(principal) = principal else {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "valid bearer token required",
        );
    };
    if state.strict_origin
        && let Some(origin) = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
        && !state
            .allowed_origins
            .iter()
            .any(|allowed| allowed == origin)
    {
        return if request.uri().path().starts_with("/v2/") {
            v2_error(
                StatusCode::FORBIDDEN,
                "ORIGIN_REJECTED",
                "request origin is not allowed",
                None,
            )
        } else {
            api_error(
                StatusCode::FORBIDDEN,
                "ORIGIN_REJECTED",
                "request origin is not allowed",
            )
        };
    }
    request.extensions_mut().insert(AuditPrincipal(principal));
    next.run(request).await
}

fn tailscale_request_principal(access: &ServeAccess, request: &Request) -> Option<String> {
    let headers = request.headers();
    let loopback_peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(peer)| peer.ip().is_loopback());
    let matching_host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| host == access.authority);
    let forwarded_https = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        == Some("https");
    let login = headers
        .get("tailscale-user-login")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|login| !login.is_empty());
    if loopback_peer && matching_host && forwarded_https {
        login.map(|login| format!("tailscale:{login}"))
    } else {
        None
    }
}

#[derive(Deserialize)]
struct PairRequest {
    code: String,
}

async fn pair_auth(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<PairRequest>,
) -> Response {
    if state.strict_origin {
        let origin = headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok());
        if !origin.is_some_and(|origin| {
            state
                .allowed_origins
                .iter()
                .any(|allowed| allowed == origin)
        }) {
            return api_error(
                StatusCode::FORBIDDEN,
                "ORIGIN_REJECTED",
                "request origin is not allowed",
            );
        }
    }
    let session =
        match redeem_pair_code(&state.token, &request.code, chrono::Utc::now().timestamp()) {
            Ok(session) => session,
            Err(_) => {
                return api_error(
                    StatusCode::UNAUTHORIZED,
                    "PAIR_INVALID",
                    "pairing token is invalid for this server startup",
                );
            }
        };
    let mut response =
        Json(json!({"apiVersion":"v1","data":{"paired":true,"expiresInSeconds":2592000}}))
            .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "observer_session={session}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000"
        ))
        .expect("session cookie contains only base64url characters"),
    );
    response
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("content-security-policy", HeaderValue::from_static(
        "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"
    ));
    response
}

async fn index() -> Response {
    match WebAssets::get("index.html") {
        Some(file) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            file.data.into_owned(),
        )
            .into_response(),
        None => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "WEB_ASSET_MISSING",
            "embedded web viewer index is missing",
        ),
    }
}

async fn web_asset(Path(path): Path<String>) -> Response {
    let asset_path = format!("assets/{path}");
    let Some(file) = WebAssets::get(&asset_path) else {
        return api_error(
            StatusCode::NOT_FOUND,
            "ROUTE_NOT_FOUND",
            "route was not found",
        );
    };
    let content_type = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };
    (
        [(header::CONTENT_TYPE, content_type)],
        file.data.into_owned(),
    )
        .into_response()
}

async fn health(State(state): State<ApiState>) -> Response {
    let seq = state.database.max_event_seq().unwrap_or(0);
    let legacy_redaction_events = state.database.legacy_redaction_event_count().unwrap_or(0);
    let sources = state.database.query_json(
        "SELECT source_id,kind,status,last_seen_at_ms,last_error_json FROM sources ORDER BY source_id", &[],
        |row| Ok(json!({
            "sourceId":row.get::<_,String>(0)?, "kind":row.get::<_,String>(1)?,
            "status":row.get::<_,String>(2)?, "lastSeenAtMs":row.get::<_,Option<i64>>(3)?,
            "lastError":row.get::<_,Option<String>>(4)?
        })),
    ).unwrap_or_default();
    let live_enabled = !state.live_modes.is_empty();
    let writer = state.writer.metrics();
    let database_state = state.database.query_json(
        "SELECT (SELECT user_version FROM pragma_user_version),
          (SELECT journal_mode FROM pragma_journal_mode),
          (SELECT COALESCE(MAX(event_seq),0) FROM raw_events WHERE thread_key<>''),
          (SELECT COALESCE(MAX(last_event_seq),0) FROM threads)",
        &[], |row| Ok(json!({"schemaVersion":row.get::<_,i64>(0)?,"journalMode":row.get::<_,String>(1)?,
          "maxEventSeq":row.get::<_,i64>(2)?,"maxProjectedEventSeq":row.get::<_,i64>(3)?})),
    ).ok().and_then(|mut rows| rows.pop()).unwrap_or_else(|| json!({}));
    let continuity = state.database.query_json(
        "SELECT COALESCE(SUM(event_count),0),COALESCE(SUM(sequence_gap_count),0),
          COALESCE(SUM(decode_error_count),0),COALESCE(SUM(unknown_event_count),0) FROM source_epochs",
        &[], |row| Ok(json!({"eventCount":row.get::<_,i64>(0)?,"sequenceGapCount":row.get::<_,i64>(1)?,
          "decodeErrorCount":row.get::<_,i64>(2)?,"unknownEventCount":row.get::<_,i64>(3)?})),
    ).ok().and_then(|mut rows| rows.pop()).unwrap_or_else(|| json!({}));
    let projection_lag = database_state["maxEventSeq"]
        .as_i64()
        .unwrap_or(0)
        .saturating_sub(database_state["maxProjectedEventSeq"].as_i64().unwrap_or(0));
    Json(ApiEnvelope::new(seq, json!({
        "status": if writer.failures > 0 || sources.iter().any(|source| matches!(source["status"].as_str(),Some("degraded"|"incompatible"))) { "degraded" } else { "healthy" },
        "ready":writer.ready,"database":{"migration":if database_state["schemaVersion"] == LATEST_SCHEMA_VERSION {"ok"} else {"mismatch"},
          "wal":database_state["journalMode"],"schemaVersion":database_state["schemaVersion"]},
        "writer":{"ready":writer.ready,"commits":writer.commits,"failures":writer.failures,"commitP95Ms":writer.commit_p95_ms},
        "ingest":{"queueDepthEvents":writer.queue_depth_events,"projectionLag":projection_lag},
        "consumers":{"active":ACTIVE_CONSUMERS.load(Ordering::Relaxed),"total":TOTAL_CONSUMERS.load(Ordering::Relaxed),
          "slowDrops":SLOW_CONSUMER_DROPS.load(Ordering::Relaxed)},"continuity":continuity,
        "sources":sources,"live":{"enabled":live_enabled,"modes":state.live_modes.as_ref()},
        "control":{"enabled":state.settings["controller"]["enabled"],
          "tailscaleMutationAccess":state.settings["controller"]["tailscaleMutationAccess"],
          "warning":state.settings["controller"]["warning"]},
        "privacy":{"redactionRuleVersion":"known-secrets-v2","legacyRedactionEvents":legacy_redaction_events,
          "warning":if legacy_redaction_events > 0 { Some("legacy records may contain values not covered by redaction v2") } else { None }}
    }))).into_response()
}

async fn sources(State(state): State<ApiState>) -> Response {
    envelope_query(
        &state,
        "SELECT s.source_id,s.kind,s.stable_identity,s.status,s.last_seen_at_ms,
          e.epoch_id,e.opened_at_ms,e.closed_at_ms,e.capability_json,e.capability_hash,e.schema_hash,e.close_reason,
          e.event_count,e.sequence_gap_count,e.decode_error_count,e.unknown_event_count,e.last_source_seq,e.last_event_at_ms,
          (SELECT CASE WHEN COUNT(*)>0 THEN COUNT(*)-1 ELSE 0 END FROM source_epochs ex WHERE ex.source_id=s.source_id),
          (SELECT COALESCE(SUM(COALESCE(ex.closed_at_ms,?1)-ex.opened_at_ms),0) FROM source_epochs ex WHERE ex.source_id=s.source_id)
         FROM sources s LEFT JOIN source_epochs e ON e.rowid=(
           SELECT e2.rowid FROM source_epochs e2 WHERE e2.source_id=s.source_id ORDER BY e2.opened_at_ms DESC LIMIT 1)
         ORDER BY s.source_id",
        &[&chrono::Utc::now().timestamp_millis()],
        |row| {
            Ok(
                json!({"sourceId":row.get::<_,String>(0)?,"kind":row.get::<_,String>(1)?,
            "stableIdentity":row.get::<_,String>(2)?,"status":row.get::<_,String>(3)?,"lastSeenAtMs":row.get::<_,Option<i64>>(4)?,
            "currentEpoch":{"epochId":row.get::<_,Option<String>>(5)?,"openedAtMs":row.get::<_,Option<i64>>(6)?,
              "closedAtMs":row.get::<_,Option<i64>>(7)?,"capabilities":parse_optional_json(row.get::<_,Option<String>>(8)?),
              "capabilityHash":row.get::<_,Option<String>>(9)?,"schemaHash":row.get::<_,Option<String>>(10)?,
              "closeReason":row.get::<_,Option<String>>(11)?,"eventCount":row.get::<_,Option<i64>>(12)?.unwrap_or(0),
              "sequenceGapCount":row.get::<_,Option<i64>>(13)?.unwrap_or(0),"decodeErrorCount":row.get::<_,Option<i64>>(14)?.unwrap_or(0),
              "unknownEventCount":row.get::<_,Option<i64>>(15)?.unwrap_or(0),"lastSourceSeq":row.get::<_,Option<i64>>(16)?,
              "lastEventAtMs":row.get::<_,Option<i64>>(17)?,
              "connectionDurationMs":match (row.get::<_,Option<i64>>(6)?,row.get::<_,Option<i64>>(7)?) { (Some(open),Some(close)) => Some(close.saturating_sub(open)), _ => None }},
              "reconnectCount":row.get::<_,i64>(18)?,"totalConnectionDurationMs":row.get::<_,i64>(19)?}),
            )
        },
    )
}

async fn projects(State(state): State<ApiState>) -> Response {
    let seq = state.database.max_event_seq().unwrap_or(0);
    let rows = state.database.query_json(
        "SELECT project_key,MIN(cwd),COUNT(*),SUM(CASE WHEN archived=0 THEN 1 ELSE 0 END),
              MAX(COALESCE(recency_at_ms,0)) FROM threads
             WHERE project_key IS NOT NULL AND project_key <> ''
             GROUP BY project_key
             ORDER BY MAX(COALESCE(recency_at_ms,0)) DESC,project_key",
        &[],
        |row| {
            let key = row.get::<_, String>(0)?;
            Ok(json!({
                "project":{"key":key,"name":project_name(&key),
                  "path":row.get::<_,Option<String>>(1)?.unwrap_or_else(|| key.clone())},
                "threadCount":row.get::<_,i64>(2)?,
                "currentThreadCount":row.get::<_,i64>(3)?,
                "lastRecencyAtMs":row.get::<_,i64>(4)?
            }))
        },
    );
    match rows {
        Ok(rows) => Json(ApiEnvelope::new(seq, rows)).into_response(),
        Err(error) => internal_error(error),
    }
}

async fn blob(
    State(state): State<ApiState>,
    Path(blob_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let record = match state.database.blob_record(&blob_id) {
        Ok(Some(record)) => record,
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "BLOB_NOT_FOUND", "blob not found"),
        Err(error) => {
            tracing::error!(error = %error, "blob lookup failed");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "blob lookup failed",
            );
        }
    };
    let range = match parse_byte_range(
        headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok()),
        record.size_bytes,
    ) {
        Ok(range) => range,
        Err(()) => {
            let mut response = api_error(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "RANGE_NOT_SATISFIABLE",
                "only one satisfiable byte range is supported",
            );
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{}", record.size_bytes)) {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            return response;
        }
    };
    let file = match state.database.open_blob(&record) {
        Ok(file) => file,
        Err(error) => {
            tracing::error!(blob_id = %record.blob_id, error = %error, "open blob failed");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "blob storage is unavailable",
            );
        }
    };
    let permit = match state.blob_downloads.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => {
            return api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "INTERNAL_ERROR",
                "blob service is shutting down",
            );
        }
    };
    let (start, end, status) = range
        .map(|(start, end)| (start, end, StatusCode::PARTIAL_CONTENT))
        .unwrap_or((0, record.size_bytes.saturating_sub(1), StatusCode::OK));
    let content_length = if record.size_bytes == 0 {
        0
    } else {
        end - start + 1
    };
    let stream = async_stream::stream! {
        let _guard = ConsumerGuard::new();
        let _permit = permit;
        let mut file = tokio::fs::File::from_std(file);
        if let Err(error) = file.seek(std::io::SeekFrom::Start(start)).await {
            yield Err::<Bytes, std::io::Error>(error);
            return;
        }
        let mut remaining = content_length;
        let mut buffer = vec![0_u8; 64 * 1024];
        while remaining > 0 {
            let read_size = remaining.min(buffer.len() as u64) as usize;
            let read = match file.read(&mut buffer[..read_size]).await {
                Ok(read) => read,
                Err(error) => {
                    yield Err::<Bytes, std::io::Error>(error);
                    return;
                }
            };
            if read == 0 {
                break;
            }
            remaining -= read as u64;
            yield Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&buffer[..read]));
        }
    };
    let content_type = if record.media_type == "application/json" {
        "application/json"
    } else {
        "application/octet-stream"
    };
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, content_length)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"observer-blob.json\"",
        );
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", record.size_bytes),
        );
    }
    builder.body(Body::from_stream(stream)).unwrap_or_else(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "failed to build blob response",
        )
    })
}

async fn threads(State(state): State<ApiState>, Query(query): Query<ThreadQuery>) -> Response {
    match query_threads(&state, query) {
        Ok(response) => response,
        Err(CursorFailure::Invalid(message)) => {
            api_error(StatusCode::BAD_REQUEST, "CURSOR_INVALID", &message)
        }
        Err(CursorFailure::Internal(error)) => internal_error(error),
    }
}

async fn thread_detail(State(state): State<ApiState>, Path(thread_key): Path<String>) -> Response {
    let rows = state.database.query_json(
        "SELECT thread_key,codex_thread_id,store_source_id,name,cwd,source,model,archived,runtime_status,
          runtime_status_stale,capture_completeness,completeness_reasons_json,created_at_ms,updated_at_ms,
          recency_at_ms,last_message_preview,last_event_seq,parent_thread_id,parent_thread_key,forked_from_id,
          forked_from_thread_key,agent_nickname,agent_role,agent_path,originator,cli_version,thread_source,
          history_mode,history_base_json,model_provider,reasoning_effort,approval_policy,approvals_reviewer_json,
          sandbox_json,active_permission_profile_json,rule_version,project_key,base_instructions_json,dynamic_tools_json,
          selected_capability_roots_json,memory_mode,subagent_history_start_ordinal,multi_agent_version,
          context_window_json FROM threads WHERE thread_key=?1",
        &[&thread_key], thread_row,
    );
    match rows {
        Ok(mut rows) if !rows.is_empty() => {
            let seq = state.database.max_event_seq().unwrap_or(0);
            let thread = rows.remove(0);
            let parent = relation_target(
                &state.database,
                thread["parentThreadKey"].as_str(),
                thread["parentThreadId"].as_str(),
            );
            let forked_from = relation_target(
                &state.database,
                thread["forkedFromThreadKey"].as_str(),
                thread["forkedFromId"].as_str(),
            );
            let children = state.database.query_json(
                "SELECT thread_key,codex_thread_id,name,runtime_status,runtime_status_stale,capture_completeness
                 FROM threads WHERE parent_thread_key=?1 OR forked_from_thread_key=?1 ORDER BY created_at_ms,thread_key",
                &[&thread_key], relation_row,
            ).unwrap_or_default();
            let store_source_id = thread["storeSourceId"].as_str().unwrap_or_default();
            let sources = state.database.query_json(
                "SELECT source_id,kind,status,last_seen_at_ms FROM sources WHERE source_id=?1 OR source_id IN
                 (SELECT DISTINCT source_id FROM raw_events WHERE thread_key=?2) ORDER BY kind,source_id",
                &[&store_source_id,&thread_key], |row| Ok(json!({
                    "sourceId":row.get::<_,String>(0)?,"kind":row.get::<_,String>(1)?,
                    "status":row.get::<_,String>(2)?,"lastSeenAtMs":row.get::<_,Option<i64>>(3)?
                })),
            ).unwrap_or_default();
            let coverage_summary = turn_coverage_summary(&state.database, &thread_key);
            let pending_requests = state.database.query_json(
                "SELECT source_id,epoch_id,request_id,request_type,state,request_version,payload_json,
                   request_event_seq,resolved_event_seq
                 FROM pending_requests WHERE thread_key=?1 ORDER BY request_event_seq DESC LIMIT 100",
                &[&thread_key], |row| {
                    let source_id = row.get::<_,String>(0)?;
                    let source_epoch = row.get::<_,String>(1)?;
                    let request_id = row.get::<_,String>(2)?;
                    let payload = row.get::<_,String>(6)?;
                    Ok(json!({"sourceId":source_id,"sourceEpoch":source_epoch,
                        "requestId":request_id,"requestType":row.get::<_,String>(3)?,"state":row.get::<_,String>(4)?,
                        "requestVersion":row.get::<_,i64>(5)?,"payload":serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null),
                        "requestEventSeq":row.get::<_,i64>(7)?,"resolvedEventSeq":row.get::<_,Option<i64>>(8)?}))
                },
            ).unwrap_or_default().into_iter().map(|mut request| {
                let request_key = encode_request_key(&RequestKey {
                    source_id: request["sourceId"].as_str().unwrap_or_default().to_string(),
                    source_epoch: request["sourceEpoch"].as_str().unwrap_or_default().to_string(),
                    request_id: request["requestId"].as_str().unwrap_or_default().to_string(),
                }, &state.token).unwrap_or_default();
                request["requestKey"] = Value::String(request_key);
                request
            }).collect::<Vec<_>>();
            let projection_conflicts = state.database.query_json(
                "SELECT conflict_id,entity_type,entity_key,field_name,live_event_seq,durable_event_seq,status,detected_at_ms,resolved_at_ms
                 FROM projection_conflicts WHERE thread_key=?1 ORDER BY detected_at_ms DESC LIMIT 100",
                &[&thread_key], |row| Ok(json!({"conflictId":row.get::<_,String>(0)?,"entityType":row.get::<_,String>(1)?,
                    "entityKey":row.get::<_,String>(2)?,"fieldName":row.get::<_,String>(3)?,"liveEventSeq":row.get::<_,i64>(4)?,
                    "durableEventSeq":row.get::<_,i64>(5)?,"status":row.get::<_,String>(6)?,
                    "detectedAtMs":row.get::<_,i64>(7)?,"resolvedAtMs":row.get::<_,Option<i64>>(8)?})),
            ).unwrap_or_default();
            Json(ApiEnvelope::new(
                seq,
                json!({
                    "thread":thread,"sources":sources,"coverageSummary":coverage_summary,
                    "pendingRequests":pending_requests,"projectionConflicts":projection_conflicts,
                    "relations":{"parent":parent,"forkedFrom":forked_from,"children":children},
                    "diagnostics":diagnostics(&state.database,&thread_key)
                }),
            ))
            .into_response()
        }
        Ok(_) => api_error(
            StatusCode::NOT_FOUND,
            "THREAD_NOT_FOUND",
            "thread was not found",
        ),
        Err(error) => internal_error(error),
    }
}

async fn turns(
    State(state): State<ApiState>,
    Path(thread_key): Path<String>,
    Query(query): Query<TurnQuery>,
) -> Response {
    match query_turns(&state, &thread_key, query) {
        Ok(response) => response,
        Err(CursorFailure::Invalid(message)) => {
            api_error(StatusCode::BAD_REQUEST, "CURSOR_INVALID", &message)
        }
        Err(CursorFailure::Internal(error)) => internal_error(error),
    }
}

async fn items(
    State(state): State<ApiState>,
    Path(thread_key): Path<String>,
    Query(query): Query<ItemQuery>,
) -> Response {
    match query_items(&state, &thread_key, query) {
        Ok(response) => response,
        Err(CursorFailure::Invalid(message)) => {
            api_error(StatusCode::BAD_REQUEST, "CURSOR_INVALID", &message)
        }
        Err(CursorFailure::Internal(error)) => internal_error(error),
    }
}

async fn thread_events(
    State(state): State<ApiState>,
    Path(thread_key): Path<String>,
    Query(query): Query<EventQuery>,
) -> Response {
    event_query(
        &state,
        query.after_event_seq.unwrap_or(0),
        query.limit,
        Some(thread_key),
        None,
        None,
    )
}

async fn events(State(state): State<ApiState>, Query(query): Query<EventQuery>) -> Response {
    event_query(
        &state,
        query.after_event_seq.unwrap_or(0),
        query.limit,
        query.thread_key,
        query.source_id,
        query.method,
    )
}

fn event_query(
    state: &ApiState,
    after: i64,
    limit: Option<usize>,
    thread: Option<String>,
    source: Option<String>,
    method: Option<String>,
) -> Response {
    match state.database.retention_low_watermark() {
        Ok(low_watermark) if after < low_watermark => {
            return api_error(
                StatusCode::GONE,
                "CURSOR_EXPIRED",
                "event cursor is older than the retention window",
            );
        }
        Err(error) => return internal_error(error),
        _ => {}
    }
    let limit = limit.unwrap_or(100).clamp(1, 200) as i64;
    let thread_param = thread.as_deref();
    let source_param = source.as_deref();
    let method_param = method.as_deref();
    envelope_query(state,
        "SELECT event_seq,event_id,source_id,epoch_id,source_seq,observed_at_ms,event_at_ms,thread_key,codex_thread_id,
          turn_id,item_id,method,phase,durability,raw_json,redaction_json,decode_status,decode_error,stored_raw_hash,blob_id
          FROM raw_events WHERE event_seq>?1 AND (?2 IS NULL OR thread_key=?2) AND (?3 IS NULL OR source_id=?3)
          AND (?4 IS NULL OR method=?4) ORDER BY event_seq LIMIT ?5",
        &[&after,&thread_param,&source_param,&method_param,&limit], event_row)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventQuery {
    after_event_seq: Option<i64>,
    limit: Option<usize>,
    thread_key: Option<String>,
    source_id: Option<String>,
    method: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadQuery {
    cursor: Option<String>,
    limit: Option<usize>,
    source_id: Option<String>,
    project: Option<String>,
    runtime_status: Option<String>,
    capture_completeness: Option<String>,
    archived: Option<bool>,
    q: Option<String>,
    sort: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TurnQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ItemQuery {
    cursor: Option<String>,
    limit: Option<usize>,
    turn_id: Option<String>,
    item_type: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchQuery {
    q: String,
    cursor: Option<String>,
    limit: Option<usize>,
}

async fn search(State(state): State<ApiState>, Query(query): Query<SearchQuery>) -> Response {
    if query.q.trim().is_empty() || query.q.chars().count() > 256 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "CURSOR_INVALID",
            "search query must contain 1 to 256 characters",
        );
    }
    match query_search(&state, query) {
        Ok(response) => response,
        Err(CursorFailure::Invalid(message)) => {
            api_error(StatusCode::BAD_REQUEST, "CURSOR_INVALID", &message)
        }
        Err(CursorFailure::Internal(error)) => internal_error(error),
    }
}

async fn capabilities(State(state): State<ApiState>) -> Response {
    let seq = state.database.max_event_seq().unwrap_or(0);
    let live_enabled = !state.live_modes.is_empty();
    Json(ApiEnvelope::new(
        seq,
        json!({
            "observerVersion":env!("CARGO_PKG_VERSION"),"observerSchemaVersion":LATEST_SCHEMA_VERSION,
            "apiVersion":"v1","readOnly":true,
            "store":{"plainJsonl":true,"zstdJsonl":true,"incrementalRescan":true,
              "contentAddressedBlobs":true,"blobRangeRequests":true},
            "live":{"enabled":live_enabled,"modes":state.live_modes.as_ref(),
              "default":"off","transport":"websocket_over_unix_socket","readOnly":true,
              "serverRequests":"persist_without_response"},
            "streams":{"sse":true,"webSocket":true},"mutationRoutes":[]
            ,"effectiveCapture":state.settings["capture"],"effectiveStorage":state.settings["storage"]
        }),
    ))
    .into_response()
}

async fn settings(State(state): State<ApiState>) -> Response {
    Json(ApiEnvelope::new(
        state.database.max_event_seq().unwrap_or(0),
        state.settings.as_ref().clone(),
    ))
    .into_response()
}

async fn sse_stream(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> Response {
    let last_event_id = match headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .map(str::parse::<i64>)
        .transpose()
    {
        Ok(value) => value,
        Err(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "CURSOR_INVALID",
                "Last-Event-ID must be an integer event sequence",
            );
        }
    };
    let mut cursor = query.after_event_seq.or(last_event_id).unwrap_or(0);
    match state.database.retention_low_watermark() {
        Ok(low_watermark) if cursor < low_watermark => {
            return api_error(
                StatusCode::GONE,
                "CURSOR_EXPIRED",
                "event cursor is older than the retention window",
            );
        }
        Err(error) => return internal_error(error),
        _ => {}
    }
    let database = state.database.clone();
    let mut committed = state.writer.subscribe();
    let filters = StreamFilters {
        thread_keys: query.thread_key.into_iter().collect(),
        source_ids: query.source_id.into_iter().collect(),
        methods: query.method.into_iter().collect(),
    };
    let stream = async_stream::stream! {
        loop {
            let rows = database.query_json(
                "SELECT event_seq,event_id,source_id,epoch_id,source_seq,observed_at_ms,event_at_ms,thread_key,codex_thread_id,
                  turn_id,item_id,method,phase,durability,raw_json,redaction_json,decode_status,decode_error,stored_raw_hash,blob_id
                  FROM raw_events WHERE event_seq>?1 ORDER BY event_seq LIMIT 200",
                &[&cursor], event_row,
            ).unwrap_or_default();
            if rows.is_empty() {
                match committed.recv().await {
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        SLOW_CONSUMER_DROPS.fetch_add(1, Ordering::Relaxed);
                        yield Ok::<Event, Infallible>(Event::default().event("error").json_data(json!({"code":"SLOW_CONSUMER","lastEventSeq":cursor})).unwrap());
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            for row in rows {
                cursor = row["eventSeq"].as_i64().unwrap_or(cursor);
                if filters.matches(&row) {
                    yield Ok::<Event, Infallible>(Event::default().id(cursor.to_string()).event("event").json_data(row).unwrap());
                }
            }
        }
    };
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("heartbeat"),
        )
        .into_response()
}

async fn ws_stream(ws: WebSocketUpgrade, State(state): State<ApiState>) -> Response {
    let committed = state.writer.subscribe();
    ws.on_upgrade(move |socket| handle_socket(socket, state.database, committed))
        .into_response()
}

async fn handle_socket(
    mut socket: WebSocket,
    database: Arc<Database>,
    mut committed: broadcast::Receiver<i64>,
) {
    let _guard = ConsumerGuard::new();
    let Ok(Some(Ok(Message::Text(text)))) =
        tokio::time::timeout(Duration::from_secs(5), socket.next()).await
    else {
        let _ = send_ws_value(
            &mut socket,
            json!({"type":"error","code":"CURSOR_INVALID","message":"subscribe frame required within 5 seconds"}),
        )
        .await;
        return;
    };
    let Ok(request) = serde_json::from_str::<WsSubscribe>(&text) else {
        let _ = send_ws_value(
            &mut socket,
            json!({"type":"error","code":"CURSOR_INVALID","message":"invalid subscribe frame"}),
        )
        .await;
        return;
    };
    if request.kind != "subscribe" || !request.filters.is_valid() {
        let _ = send_ws_value(
            &mut socket,
            json!({"type":"error","code":"CURSOR_INVALID","message":"invalid subscribe type or filters"}),
        )
        .await;
        return;
    }
    let mut cursor = request.after_event_seq.unwrap_or(0);
    if database
        .retention_low_watermark()
        .is_ok_and(|low_watermark| cursor < low_watermark)
    {
        let _ = send_ws_value(&mut socket, json!({"type":"error","code":"CURSOR_EXPIRED"})).await;
        return;
    }
    if !send_ws_value(
        &mut socket,
        json!({"type":"subscribed","afterEventSeq":cursor}),
    )
    .await
    {
        return;
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.tick().await;
    loop {
        let rows = database.query_json(
            "SELECT event_seq,event_id,source_id,epoch_id,source_seq,observed_at_ms,event_at_ms,thread_key,codex_thread_id,
              turn_id,item_id,method,phase,durability,raw_json,redaction_json,decode_status,decode_error,stored_raw_hash,blob_id
              FROM raw_events WHERE event_seq>?1 ORDER BY event_seq LIMIT 200", &[&cursor], event_row,
        ).unwrap_or_default();
        for row in rows {
            cursor = row["eventSeq"].as_i64().unwrap_or(cursor);
            if !request.filters.matches(&row) {
                continue;
            }
            let frame = json!({"type":"event","eventSeq":cursor,"data":row});
            if !send_ws_value(&mut socket, frame).await {
                close_slow_consumer(&mut socket, cursor).await;
                return;
            }
        }
        tokio::select! {
            event = committed.recv() => {
                match event {
                    Ok(_) => {},
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        SLOW_CONSUMER_DROPS.fetch_add(1, Ordering::Relaxed);
                        close_slow_consumer(&mut socket, cursor).await;
                        return;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            },
            _ = heartbeat.tick() => {
                if !send_ws_value(&mut socket, json!({"type":"heartbeat","eventSeq":cursor})).await {
                    close_slow_consumer(&mut socket, cursor).await;
                    return;
                }
            },
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    _ => {}
                }
            }
        }
    }
}

async fn send_ws_value(socket: &mut WebSocket, value: Value) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(10),
            socket.send(Message::Text(value.to_string().into()))
        )
        .await,
        Ok(Ok(()))
    )
}

async fn close_slow_consumer(socket: &mut WebSocket, cursor: i64) {
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        socket.send(Message::Text(
            json!({"type":"error","code":"SLOW_CONSUMER","lastEventSeq":cursor})
                .to_string()
                .into(),
        )),
    )
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.close()).await;
}

fn envelope_query(
    state: &ApiState,
    sql: &str,
    parameters: &[&dyn rusqlite::ToSql],
    mapper: fn(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
) -> Response {
    match state.database.query_json(sql, parameters, mapper) {
        Ok(rows) => Json(ApiEnvelope::new(
            state.database.max_event_seq().unwrap_or(0),
            rows,
        ))
        .into_response(),
        Err(error) => internal_error(error),
    }
}

fn query_threads(state: &ApiState, query: ThreadQuery) -> Result<Response, CursorFailure> {
    if query.sort.as_deref().unwrap_or("recency_desc") != "recency_desc" {
        return Err(CursorFailure::Invalid(
            "only sort=recency_desc is supported".into(),
        ));
    }
    if query
        .q
        .as_ref()
        .is_some_and(|value| value.trim().is_empty() || value.chars().count() > 256)
    {
        return Err(CursorFailure::Invalid(
            "q must contain 1 to 256 characters".into(),
        ));
    }
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let fingerprint = thread_query_fingerprint(&query);
    let decoded_cursor = if let Some(cursor) = query.cursor.as_deref() {
        let decoded = decode_cursor(cursor, &state.token)
            .map_err(|message| CursorFailure::Invalid(message.to_string()))?;
        if decoded.endpoint != "threads" || decoded.query_fingerprint != fingerprint {
            return Err(CursorFailure::Invalid(
                "cursor does not match the current query".into(),
            ));
        }
        Some(decoded)
    } else {
        None
    };
    let as_of = decoded_cursor
        .as_ref()
        .map(|cursor| cursor.as_of_event_seq)
        .unwrap_or(state.database.max_event_seq()?);

    let mut sql = String::from(
        "SELECT thread_key,codex_thread_id,store_source_id,name,cwd,source,model,archived,runtime_status,
          runtime_status_stale,capture_completeness,completeness_reasons_json,created_at_ms,updated_at_ms,
          recency_at_ms,last_message_preview,last_event_seq,parent_thread_id,parent_thread_key,forked_from_id,
          forked_from_thread_key,agent_nickname,agent_role,agent_path,originator,cli_version,thread_source,
          history_mode,history_base_json,model_provider,reasoning_effort,approval_policy,approvals_reviewer_json,
          sandbox_json,active_permission_profile_json,rule_version,project_key,base_instructions_json,dynamic_tools_json,
          selected_capability_roots_json,memory_mode,subagent_history_start_ordinal,multi_agent_version,
          context_window_json FROM threads WHERE last_event_seq <= ?",
    );
    let mut parameters = vec![SqlValue::Integer(as_of)];
    if let Some(source_id) = query.source_id {
        sql.push_str(" AND store_source_id = ?");
        parameters.push(SqlValue::Text(source_id));
    }
    if let Some(project) = query.project {
        sql.push_str(" AND project_key = ?");
        parameters.push(SqlValue::Text(project));
    }
    if let Some(status) = query.runtime_status {
        sql.push_str(" AND runtime_status = ?");
        parameters.push(SqlValue::Text(status));
    }
    if let Some(completeness) = query.capture_completeness {
        sql.push_str(" AND capture_completeness = ?");
        parameters.push(SqlValue::Text(completeness));
    }
    if let Some(archived) = query.archived {
        sql.push_str(" AND archived = ?");
        parameters.push(SqlValue::Integer(i64::from(archived)));
    }
    if let Some(search) = query.q {
        sql.push_str(
            " AND thread_key IN (SELECT thread_key FROM search_index WHERE search_index MATCH ?)",
        );
        parameters.push(SqlValue::Text(search_expression(&search)));
    }
    if let Some(cursor) = &decoded_cursor {
        sql.push_str(
            " AND (COALESCE(recency_at_ms,0) < ? OR (COALESCE(recency_at_ms,0) = ? AND thread_key > ?))",
        );
        parameters.push(SqlValue::Integer(cursor.last_recency_at_ms));
        parameters.push(SqlValue::Integer(cursor.last_recency_at_ms));
        parameters.push(SqlValue::Text(cursor.last_thread_key.clone()));
    }
    sql.push_str(" ORDER BY COALESCE(recency_at_ms,0) DESC,thread_key ASC LIMIT ?");
    parameters.push(SqlValue::Integer((limit + 1) as i64));
    let mut rows = state
        .database
        .query_json_owned(&sql, parameters, thread_row)?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if has_more {
        let last = rows.last().expect("non-empty page at limit");
        Some(encode_cursor(
            &ThreadCursor {
                endpoint: "threads".into(),
                query_fingerprint: fingerprint,
                as_of_event_seq: as_of,
                last_recency_at_ms: last["recencyAtMs"].as_i64().unwrap_or(0),
                last_thread_key: last["threadKey"].as_str().unwrap_or_default().to_string(),
            },
            &state.token,
        )?)
    } else {
        None
    };
    Ok(Json(ApiEnvelope::with_cursor(as_of, rows, next_cursor)).into_response())
}

fn query_turns(
    state: &ApiState,
    thread_key: &str,
    query: TurnQuery,
) -> Result<Response, CursorFailure> {
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let fingerprint = query_fingerprint(&json!({"threadKey":thread_key}));
    let endpoint = format!("turns:{thread_key}");
    let decoded_cursor = decode_bound_page_cursor(
        query.cursor.as_deref(),
        &endpoint,
        &fingerprint,
        &state.token,
    )?;
    let as_of = decoded_cursor
        .as_ref()
        .map(|cursor| cursor.as_of_event_seq)
        .unwrap_or(state.database.max_event_seq()?);
    let mut sql = String::from(
        "SELECT turn_id,status,capture_completeness,completeness_reasons_json,coverage_json,started_at_ms,
          completed_at_ms,execution_context_json,projection_json,last_event_seq FROM turns
          WHERE thread_key=? AND last_event_seq<=?",
    );
    let mut parameters = vec![
        SqlValue::Text(thread_key.to_string()),
        SqlValue::Integer(as_of),
    ];
    if let Some(cursor) = &decoded_cursor {
        sql.push_str(
            " AND (COALESCE(started_at_ms,0)>? OR (COALESCE(started_at_ms,0)=? AND turn_id>?))",
        );
        parameters.push(SqlValue::Integer(cursor.last_sort));
        parameters.push(SqlValue::Integer(cursor.last_sort));
        parameters.push(SqlValue::Text(cursor.last_key.clone()));
    }
    sql.push_str(" ORDER BY COALESCE(started_at_ms,0),turn_id LIMIT ?");
    parameters.push(SqlValue::Integer((limit + 1) as i64));
    let mut rows = state
        .database
        .query_json_owned(&sql, parameters, turn_row)?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if has_more {
        let last = rows.last().expect("non-empty page at limit");
        Some(encode_page_cursor(
            &PageCursor {
                endpoint,
                query_fingerprint: fingerprint,
                as_of_event_seq: as_of,
                last_sort: last["startedAtMs"].as_i64().unwrap_or(0),
                last_key: last["turnId"].as_str().unwrap_or_default().to_string(),
                last_secondary: None,
            },
            &state.token,
        )?)
    } else {
        None
    };
    Ok(Json(ApiEnvelope::with_cursor(as_of, rows, next_cursor)).into_response())
}

fn query_items(
    state: &ApiState,
    thread_key: &str,
    query: ItemQuery,
) -> Result<Response, CursorFailure> {
    let limit = query.limit.unwrap_or(200).clamp(1, 200);
    let fingerprint = query_fingerprint(&json!({
        "threadKey":thread_key,
        "turnId":query.turn_id,
        "itemType":query.item_type
    }));
    let endpoint = format!("items:{thread_key}");
    let decoded_cursor = decode_bound_page_cursor(
        query.cursor.as_deref(),
        &endpoint,
        &fingerprint,
        &state.token,
    )?;
    let as_of = decoded_cursor
        .as_ref()
        .map(|cursor| cursor.as_of_event_seq)
        .unwrap_or(state.database.max_event_seq()?);
    let mut sql = String::from(
        "SELECT turn_scope,item_id,turn_id,item_type,status,started_at_ms,completed_at_ms,summary_text,
          projection_json,provenance_json,last_event_seq FROM items WHERE thread_key=? AND last_event_seq<=?",
    );
    let mut parameters = vec![
        SqlValue::Text(thread_key.to_string()),
        SqlValue::Integer(as_of),
    ];
    if let Some(turn_id) = query.turn_id {
        sql.push_str(" AND turn_id=?");
        parameters.push(SqlValue::Text(turn_id));
    }
    if let Some(item_type) = query.item_type {
        sql.push_str(" AND item_type=?");
        parameters.push(SqlValue::Text(item_type));
    }
    if let Some(cursor) = &decoded_cursor {
        let secondary = cursor.last_secondary.as_deref().ok_or_else(|| {
            CursorFailure::Invalid("item cursor is missing its secondary key".into())
        })?;
        sql.push_str(
            " AND (COALESCE(started_at_ms,0)>?
             OR (COALESCE(started_at_ms,0)=? AND turn_scope>?)
             OR (COALESCE(started_at_ms,0)=? AND turn_scope=? AND item_id>?))",
        );
        parameters.push(SqlValue::Integer(cursor.last_sort));
        parameters.push(SqlValue::Integer(cursor.last_sort));
        parameters.push(SqlValue::Text(cursor.last_key.clone()));
        parameters.push(SqlValue::Integer(cursor.last_sort));
        parameters.push(SqlValue::Text(cursor.last_key.clone()));
        parameters.push(SqlValue::Text(secondary.to_string()));
    }
    sql.push_str(" ORDER BY COALESCE(started_at_ms,0),turn_scope,item_id LIMIT ?");
    parameters.push(SqlValue::Integer((limit + 1) as i64));
    let mut rows = state
        .database
        .query_json_owned(&sql, parameters, item_row)?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if has_more {
        let last = rows.last().expect("non-empty page at limit");
        Some(encode_page_cursor(
            &PageCursor {
                endpoint,
                query_fingerprint: fingerprint,
                as_of_event_seq: as_of,
                last_sort: last["startedAtMs"].as_i64().unwrap_or(0),
                last_key: last["turnScope"].as_str().unwrap_or_default().to_string(),
                last_secondary: Some(last["itemId"].as_str().unwrap_or_default().to_string()),
            },
            &state.token,
        )?)
    } else {
        None
    };
    Ok(Json(ApiEnvelope::with_cursor(as_of, rows, next_cursor)).into_response())
}

fn query_search(state: &ApiState, query: SearchQuery) -> Result<Response, CursorFailure> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let fingerprint = query_fingerprint(&json!({"q":query.q}));
    let decoded_cursor = decode_bound_page_cursor(
        query.cursor.as_deref(),
        "search",
        &fingerprint,
        &state.token,
    )?;
    let as_of = decoded_cursor
        .as_ref()
        .map(|cursor| cursor.as_of_event_seq)
        .unwrap_or(state.database.max_event_seq()?);
    let mut sql = String::from(
        "SELECT search_index.entity_key,search_index.thread_key,search_index.item_id,i.turn_id,
          snippet(search_index,3,'','', ' … ',20),bm25(search_index),i.last_event_seq
         FROM search_index JOIN items i
           ON search_index.thread_key=i.thread_key AND search_index.item_id=i.item_id
          AND search_index.entity_key=i.thread_key || ':' || i.turn_scope || ':' || i.item_id
         WHERE search_index MATCH ? AND i.last_event_seq<=?",
    );
    let mut parameters = vec![
        SqlValue::Text(search_expression(&query.q)),
        SqlValue::Integer(as_of),
    ];
    if let Some(cursor) = &decoded_cursor {
        sql.push_str(
            " AND (i.last_event_seq<? OR (i.last_event_seq=? AND search_index.entity_key>?))",
        );
        parameters.push(SqlValue::Integer(cursor.last_sort));
        parameters.push(SqlValue::Integer(cursor.last_sort));
        parameters.push(SqlValue::Text(cursor.last_key.clone()));
    }
    sql.push_str(" ORDER BY i.last_event_seq DESC,search_index.entity_key LIMIT ?");
    parameters.push(SqlValue::Integer((limit + 1) as i64));
    let mut rows = state
        .database
        .query_json_owned(&sql, parameters, search_row)?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if has_more {
        let last = rows.last().expect("non-empty page at limit");
        Some(encode_page_cursor(
            &PageCursor {
                endpoint: "search".into(),
                query_fingerprint: fingerprint,
                as_of_event_seq: as_of,
                last_sort: last["lastEventSeq"].as_i64().unwrap_or(0),
                last_key: last["entityKey"].as_str().unwrap_or_default().to_string(),
                last_secondary: None,
            },
            &state.token,
        )?)
    } else {
        None
    };
    Ok(Json(ApiEnvelope::with_cursor(as_of, rows, next_cursor)).into_response())
}

fn thread_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let cwd: Option<String> = row.get(4)?;
    let project_key_value: Option<String> = row.get(36)?;
    let project = project_key_value
        .filter(|value| !value.is_empty())
        .map(|key| {
            json!({
                "key":key,"name":project_name(&key),
                "path":cwd.clone().unwrap_or_else(|| key.clone())
            })
        });

    let base_instructions = parse_optional_json(row.get(37)?);
    let dynamic_tools = parse_optional_json(row.get(38)?);
    let selected_capability_roots = parse_optional_json(row.get(39)?);
    let memory_mode: Option<String> = row.get(40)?;
    let subagent_history_start_ordinal: Option<i64> = row.get(41)?;
    let multi_agent_version: Option<String> = row.get(42)?;
    let context_window = parse_optional_json(row.get(43)?);

    Ok(json!({
        "threadKey":row.get::<_,String>(0)?,"codexThreadId":row.get::<_,String>(1)?,"storeSourceId":row.get::<_,String>(2)?,
        "name":row.get::<_,Option<String>>(3)?,"cwdDisplay":row.get::<_,Option<String>>(4)?,"source":row.get::<_,Option<String>>(5)?,
        "model":row.get::<_,Option<String>>(6)?,"archived":row.get::<_,bool>(7)?,"status":row.get::<_,Option<String>>(8)?,
        "stale":row.get::<_,bool>(9)?,"captureCompleteness":row.get::<_,String>(10)?,
        "completenessReasons":parse_json(row.get::<_,String>(11)?),"createdAtMs":row.get::<_,Option<i64>>(12)?,
        "updatedAtMs":row.get::<_,Option<i64>>(13)?,"recencyAtMs":row.get::<_,Option<i64>>(14)?,
        "lastMessagePreview":row.get::<_,Option<String>>(15)?,"lastEventSeq":row.get::<_,i64>(16)?
        ,"parentThreadId":row.get::<_,Option<String>>(17)?,"parentThreadKey":row.get::<_,Option<String>>(18)?,
        "forkedFromId":row.get::<_,Option<String>>(19)?,"forkedFromThreadKey":row.get::<_,Option<String>>(20)?,
        "agentNickname":row.get::<_,Option<String>>(21)?,"agentRole":row.get::<_,Option<String>>(22)?,
        "agentPath":row.get::<_,Option<String>>(23)?,"originator":row.get::<_,Option<String>>(24)?,
        "cliVersion":row.get::<_,Option<String>>(25)?,"threadSource":row.get::<_,Option<String>>(26)?,
        "historyMode":row.get::<_,Option<String>>(27)?,"historyBase":parse_optional_json(row.get::<_,Option<String>>(28)?),
        "modelProvider":row.get::<_,Option<String>>(29)?,"reasoningEffort":row.get::<_,Option<String>>(30)?,
        "approvalPolicy":row.get::<_,Option<String>>(31)?,"approvalsReviewer":parse_optional_json(row.get::<_,Option<String>>(32)?),
        "sandbox":parse_optional_json(row.get::<_,Option<String>>(33)?),
        "activePermissionProfile":parse_optional_json(row.get::<_,Option<String>>(34)?),"ruleVersion":row.get::<_,String>(35)?,
        "project":project,
        "context":{
            "session":{
                "baseInstructions":base_instructions,
                "dynamicTools":dynamic_tools,
                "selectedCapabilityRoots":selected_capability_roots,
                "memoryMode":memory_mode,
                "subagentHistoryStartOrdinal":subagent_history_start_ordinal,
                "multiAgentVersion":multi_agent_version,
                "contextWindow":context_window,
                "agentNickname":row.get::<_,Option<String>>(21)?,
                "agentRole":row.get::<_,Option<String>>(22)?,
                "agentPath":row.get::<_,Option<String>>(23)?,
                "originator":row.get::<_,Option<String>>(24)?,
                "cliVersion":row.get::<_,Option<String>>(25)?,
                "threadSource":row.get::<_,Option<String>>(26)?,
                "historyMode":row.get::<_,Option<String>>(27)?,
                "historyBase":parse_optional_json(row.get::<_,Option<String>>(28)?),
                "modelProvider":row.get::<_,Option<String>>(29)?
            },
            "runtime":{
                "cwd":cwd,
                "source":row.get::<_,Option<String>>(5)?,
                "model":row.get::<_,Option<String>>(6)?,
                "reasoningEffort":row.get::<_,Option<String>>(30)?,
                "approvalPolicy":row.get::<_,Option<String>>(31)?,
                "approvalsReviewer":parse_optional_json(row.get::<_,Option<String>>(32)?),
                "sandbox":parse_optional_json(row.get::<_,Option<String>>(33)?),
                "activePermissionProfile":parse_optional_json(row.get::<_,Option<String>>(34)?)
            }
        }
    }))
}

fn turn_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let execution_context = parse_optional_json(row.get::<_, Option<String>>(7)?);
    let context = turn_context_view(&execution_context);
    Ok(json!({
        "turnId":row.get::<_,String>(0)?,"status":row.get::<_,String>(1)?,
        "captureCompleteness":row.get::<_,String>(2)?,"completenessReasons":parse_json(row.get::<_,String>(3)?),
        "coverage":parse_json(row.get::<_,String>(4)?),"startedAtMs":row.get::<_,Option<i64>>(5)?,
        "completedAtMs":row.get::<_,Option<i64>>(6)?,"executionContext":execution_context,
        "context":context,
        "raw":parse_json(row.get::<_,String>(8)?),"lastEventSeq":row.get::<_,i64>(9)?
    }))
}

fn turn_context_view(raw: &Value) -> Value {
    json!({
        "cwd":raw.get("cwd"),
        "workspaceRoots":raw.get("workspace_roots"),
        "currentDate":raw.get("current_date"),
        "timezone":raw.get("timezone"),
        "approvalPolicy":raw.get("approval_policy"),
        "approvalsReviewer":raw.get("approvals_reviewer"),
        "sandbox":raw.get("sandbox_policy"),
        "permissionProfile":raw.get("permission_profile"),
        "network":raw.get("network"),
        "model":raw.get("model"),
        "effort":raw.get("effort"),
        "personality":raw.get("personality"),
        "collaborationMode":raw.get("collaboration_mode"),
        "multiAgentVersion":raw.get("multi_agent_version")
    })
}

fn item_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "turnScope":row.get::<_,String>(0)?,"itemId":row.get::<_,String>(1)?,"turnId":row.get::<_,Option<String>>(2)?,
        "itemType":row.get::<_,String>(3)?,"status":row.get::<_,String>(4)?,"startedAtMs":row.get::<_,Option<i64>>(5)?,
        "completedAtMs":row.get::<_,Option<i64>>(6)?,"summaryText":row.get::<_,Option<String>>(7)?,
        "raw":parse_json(row.get::<_,String>(8)?),"provenance":parse_json(row.get::<_,String>(9)?),"lastEventSeq":row.get::<_,i64>(10)?
    }))
}

fn search_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "entityKey":row.get::<_,String>(0)?,"threadKey":row.get::<_,String>(1)?,
        "itemId":row.get::<_,String>(2)?,"turnId":row.get::<_,Option<String>>(3)?,
        "snippet":row.get::<_,String>(4)?,"score":row.get::<_,f64>(5)?,
        "lastEventSeq":row.get::<_,i64>(6)?
    }))
}

fn relation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "threadKey":row.get::<_,String>(0)?,"codexThreadId":row.get::<_,String>(1)?,
        "name":row.get::<_,Option<String>>(2)?,"status":row.get::<_,Option<String>>(3)?,
        "stale":row.get::<_,bool>(4)?,"captureCompleteness":row.get::<_,String>(5)?,"resolved":true
    }))
}

fn relation_target(
    database: &Database,
    thread_key: Option<&str>,
    thread_id: Option<&str>,
) -> Value {
    let Some(thread_key) = thread_key else {
        return Value::Null;
    };
    database
        .query_json(
            "SELECT thread_key,codex_thread_id,name,runtime_status,runtime_status_stale,capture_completeness
             FROM threads WHERE thread_key=?1",
            &[&thread_key],
            relation_row,
        )
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .unwrap_or_else(|| {
            json!({"threadKey":thread_key,"codexThreadId":thread_id,"resolved":false})
        })
}

fn turn_coverage_summary(database: &Database, thread_key: &str) -> Value {
    let rows = database
        .query_json(
            "SELECT capture_completeness,COUNT(*) FROM turns WHERE thread_key=?1
             GROUP BY capture_completeness ORDER BY capture_completeness",
            &[&thread_key],
            |row| {
                Ok(json!({
                    "captureCompleteness":row.get::<_,String>(0)?,"count":row.get::<_,i64>(1)?
                }))
            },
        )
        .unwrap_or_default();
    json!({"turns":rows})
}

fn event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "eventSeq":row.get::<_,i64>(0)?,"eventId":row.get::<_,String>(1)?,"sourceId":row.get::<_,String>(2)?,
        "sourceEpoch":row.get::<_,String>(3)?,"sourceSeq":row.get::<_,i64>(4)?,"observedAtMs":row.get::<_,i64>(5)?,
        "eventAtMs":row.get::<_,Option<i64>>(6)?,"threadKey":row.get::<_,String>(7)?,"codexThreadId":row.get::<_,String>(8)?,
        "turnId":row.get::<_,Option<String>>(9)?,"itemId":row.get::<_,Option<String>>(10)?,"method":row.get::<_,String>(11)?,
        "phase":row.get::<_,String>(12)?,"durability":row.get::<_,String>(13)?,"raw":parse_json(row.get::<_,String>(14)?),
        "redaction":parse_json(row.get::<_,String>(15)?),"decodeStatus":row.get::<_,String>(16)?,
        "decodeError":row.get::<_,Option<String>>(17)?,"storedRawHash":row.get::<_,String>(18)?,
        "blobId":row.get::<_,Option<String>>(19)?
    }))
}

fn diagnostics(database: &Database, thread_key: &str) -> Value {
    let rows = database.query_json(
        "SELECT SUM(CASE WHEN decode_status='error' THEN 1 ELSE 0 END),
          SUM(CASE WHEN decode_status='unknown' THEN 1 ELSE 0 END),
          (SELECT COUNT(*) FROM projection_conflicts c WHERE c.thread_key=?1 AND c.status='active')
          FROM raw_events WHERE thread_key=?1",
        &[&thread_key], |row| Ok(json!({"decodeErrors":row.get::<_,Option<i64>>(0)?.unwrap_or(0),
            "unknownVariants":row.get::<_,Option<i64>>(1)?.unwrap_or(0),"conflicts":row.get::<_,i64>(2)?})),
    ).unwrap_or_default();
    rows.into_iter()
        .next()
        .unwrap_or_else(|| json!({"decodeErrors":0,"unknownVariants":0,"conflicts":0}))
}

fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    let retryable = matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
    );
    (
        status,
        Json(
            json!({"apiVersion":"v1","error":{"code":code,"message":message,"details":{},
          "requestId":uuid::Uuid::now_v7().to_string(),"retryable":retryable}}),
        ),
    )
        .into_response()
}

fn internal_error(error: anyhow::Error) -> Response {
    tracing::error!(error = %error, "API query failed");
    let busy = error.chain().any(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .is_some_and(|error| {
                matches!(error,
            rusqlite::Error::SqliteFailure(code, _) if matches!(code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
            })
    });
    if busy {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "OBSERVER_BUSY",
            "Observer database is busy; retry later",
        );
    }
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "INTERNAL_ERROR",
        "internal query error",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::Importer;
    use anyhow::{Context as _, bail};
    use tempfile::TempDir;
    use tower::ServiceExt;

    #[test]
    fn token_comparison_checks_length_and_content() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"other"));
        assert!(!constant_time_eq(b"token", b"token-long"));
    }

    #[tokio::test]
    async fn controller_sources_is_authenticated_and_reports_disabled_empty_state() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let state = ApiState {
            writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
            controller: ControllerRegistry::default(),
            database,
            token: Arc::new("read-token".into()),
            fingerprint_key: [7; 32],
            strict_origin: true,
            allowed_origins: Arc::new(vec!["http://127.0.0.1:4765".into()]),
            live_modes: Arc::new(Vec::new()),
            blob_downloads: Arc::new(Semaphore::new(1)),
            settings: Arc::new(json!({"controller":{"enabled":false}})),
            tailscale: None,
        };
        let app = Router::new()
            .route("/v2/control/sources", get(controller_sources))
            .route_layer(axum_middleware::from_fn_with_state(
                state.clone(),
                authorize,
            ))
            .with_state(state);

        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v2/control/sources")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v2/control/sources")
                    .header(header::AUTHORIZATION, "Bearer read-token")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await?)?;
        assert_eq!(body["apiVersion"], "v2");
        assert_eq!(body["controllerEnabled"], false);
        assert_eq!(body["data"], json!([]));
        Ok(())
    }

    fn gateway_test_state(
        database: Arc<Database>,
        token: &str,
        tailscale: Option<ServeAccess>,
    ) -> Result<ApiState> {
        Ok(ApiState {
            writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
            controller: ControllerRegistry::default(),
            database,
            token: Arc::new(token.into()),
            fingerprint_key: [7; 32],
            strict_origin: true,
            allowed_origins: Arc::new(vec![
                "http://127.0.0.1:4765".into(),
                "https://observer.example.ts.net".into(),
            ]),
            live_modes: Arc::new(Vec::new()),
            blob_downloads: Arc::new(Semaphore::new(1)),
            settings: Arc::new(json!({"controller":{"enabled":true}})),
            tailscale: tailscale.map(Arc::new),
        })
    }

    fn gateway_test_app(state: ApiState) -> Router {
        Router::new()
            .route(
                "/v2/commands",
                get(list_gateway_commands).post(create_gateway_command),
            )
            .route("/v2/commands/{command_id}", get(get_gateway_command))
            .route("/v2/control/catalog", get(controller_catalog))
            .route("/v2/stream", get(v2_stream))
            .route("/v2/threads", post(create_gateway_thread))
            .route("/v2/threads/{thread_key}/inputs", post(create_thread_input))
            .route(
                "/v2/requests/{request_key}/actions",
                post(create_pending_request_action),
            )
            .route("/v2/uploads/images", post(upload_image))
            .route_layer(axum_middleware::from_fn_with_state(
                state.clone(),
                authorize,
            ))
            .with_state(state)
    }

    async fn response_json(response: Response) -> Result<Value> {
        Ok(serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX).await?,
        )?)
    }

    async fn first_sse_frame(response: Response) -> Result<String> {
        let mut stream = response.into_body().into_data_stream();
        let mut frame = String::new();
        while !frame.contains("\n\n") {
            let chunk = tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .context("timed out waiting for SSE data")?
                .context("SSE stream ended before an event")??;
            frame.push_str(std::str::from_utf8(&chunk)?);
        }
        Ok(frame)
    }

    #[tokio::test]
    async fn command_api_requires_origin_is_idempotent_and_audits_shared_local_auth() -> Result<()>
    {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let token = URL_SAFE_NO_PAD.encode([4_u8; 32]);
        let app = gateway_test_app(gateway_test_state(database.clone(), &token, None)?);
        let body = json!({
            "capability":"turn.start",
            "target":{"sourceId":"source","sourceEpoch":"epoch","threadKey":"thread-key","codexThreadId":"thread"},
            "input":{"text":"sensitive fixture message","clientUserMessageId":"client-message"}
        })
        .to_string();

        let missing_origin = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v2/commands")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header("idempotency-key", "bearer-key-0001")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.clone()))?,
            )
            .await?;
        assert_eq!(missing_origin.status(), StatusCode::FORBIDDEN);

        let request = |body: String| -> Result<Request> {
            Ok(Request::builder()
                .method("POST")
                .uri("/v2/commands")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("idempotency-key", "bearer-key-0001")
                .header(header::ORIGIN, "http://127.0.0.1:4765")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))?)
        };
        let response = app.clone().oneshot(request(body.clone())?).await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let first = response_json(response).await?;
        assert_eq!(first["error"]["code"], "SOURCE_NOT_LIVE");
        let command_id = first["error"]["details"]["commandId"]
            .as_str()
            .context("missing command id")?
            .to_string();

        let replay = app.clone().oneshot(request(body.clone())?).await?;
        assert_eq!(replay.status(), StatusCode::OK);
        let replay = response_json(replay).await?;
        assert_eq!(replay["data"]["commandId"], command_id);
        assert_eq!(replay["data"]["state"], "rejected");

        let conflict = body.replace("sensitive fixture message", "different fixture message");
        let conflict = app.clone().oneshot(request(conflict)?).await?;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(conflict).await?["error"]["code"],
            "IDEMPOTENCY_CONFLICT"
        );

        let pair = auth::generate_pair_code(&token)?;
        let session = redeem_pair_code(&token, &pair, chrono::Utc::now().timestamp())?;
        let cookie = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v2/commands")
                    .header(header::COOKIE, format!("observer_session={session}"))
                    .header("idempotency-key", "cookie-key-0001")
                    .header(header::ORIGIN, "http://127.0.0.1:4765")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))?,
            )
            .await?;
        assert_eq!(cookie.status(), StatusCode::SERVICE_UNAVAILABLE);

        let connection = database.connect()?;
        let counts: (i64, i64, i64) = connection.query_row(
            "SELECT (SELECT COUNT(*) FROM gateway_commands),
                    (SELECT COUNT(*) FROM command_transitions),
                    (SELECT COUNT(*) FROM control_audit)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(counts, (2, 6, 6));
        let principals: String = connection.query_row(
            "SELECT group_concat(DISTINCT principal_id) FROM gateway_commands",
            [],
            |row| row.get(0),
        )?;
        assert!(principals.contains("local_bearer"));
        assert!(principals.contains("local_cookie"));
        let summaries: String = connection.query_row(
            "SELECT group_concat(input_summary_json) FROM gateway_commands",
            [],
            |row| row.get(0),
        )?;
        assert!(!summaries.contains("sensitive fixture message"));
        drop(connection);

        let first_page = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v2/commands?state=rejected&limit=1")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(first_page.status(), StatusCode::OK);
        let first_page = response_json(first_page).await?;
        assert_eq!(first_page["data"].as_array().map(Vec::len), Some(1));
        let cursor = first_page["nextCursor"]
            .as_str()
            .context("missing command cursor")?;
        let second_page = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v2/commands?state=rejected&limit=1&cursor={cursor}"
                    ))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(second_page.status(), StatusCode::OK);
        let second_page = response_json(second_page).await?;
        assert_eq!(second_page["data"].as_array().map(Vec::len), Some(1));
        assert!(second_page["nextCursor"].is_null());

        let mismatched = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v2/commands?state=failed&limit=1&cursor={cursor}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(mismatched.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(mismatched).await?["error"]["code"],
            "CURSOR_INVALID"
        );
        Ok(())
    }

    #[tokio::test]
    async fn request_action_requires_signed_key_origin_epoch_and_keeps_content_out_of_audit()
    -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let connection = database.connect()?;
        connection.execute(
            "INSERT INTO threads(thread_key,store_source_id,codex_thread_id,archived,
               capture_completeness,completeness_reasons_json,projection_json,provenance_json,last_event_seq)
             VALUES ('thread-key','store-source','thread',0,'metadata_only','[]','{}','{}',0)",
            [],
        )?;
        connection.execute(
            "INSERT INTO pending_requests(source_id,epoch_id,request_id,thread_key,request_type,
               state,request_event_seq,payload_json,request_version)
             VALUES ('source','epoch','request','thread-key','mcp_elicitation','pending',1,
               '{\"requestedSchema\":{\"type\":\"object\"}}',2)",
            [],
        )?;
        drop(connection);
        let token = URL_SAFE_NO_PAD.encode([21_u8; 32]);
        let request_key = encode_request_key(
            &RequestKey {
                source_id: "source".into(),
                source_epoch: "epoch".into(),
                request_id: "request".into(),
            },
            &token,
        )?;
        let app = gateway_test_app(gateway_test_state(database.clone(), &token, None)?);
        let body = json!({
            "sourceEpoch":"epoch",
            "expectedRequestVersion":2,
            "action":{"type":"mcpElicitation","action":"accept","content":{"value":"PRIVATE-MCP-CONTENT"}}
        })
        .to_string();

        let missing_origin = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v2/requests/{request_key}/actions"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header("idempotency-key", "request-key-0001")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.clone()))?,
            )
            .await?;
        assert_eq!(missing_origin.status(), StatusCode::FORBIDDEN);

        let mut tampered = request_key.clone().into_bytes();
        tampered[0] = if tampered[0] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered)?;
        let invalid_key = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v2/requests/{tampered}/actions"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::ORIGIN, "http://127.0.0.1:4765")
                    .header("idempotency-key", "request-key-0002")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.clone()))?,
            )
            .await?;
        assert_eq!(invalid_key.status(), StatusCode::BAD_REQUEST);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v2/requests/{request_key}/actions"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::ORIGIN, "http://127.0.0.1:4765")
                    .header("idempotency-key", "request-key-0003")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let error = response_json(response).await?;
        assert_eq!(error["error"]["code"], "SOURCE_NOT_LIVE");

        let connection = database.connect()?;
        let stored: String = connection.query_row(
            "SELECT group_concat(value,' ') FROM (
               SELECT input_summary_json AS value FROM gateway_commands
               UNION ALL SELECT input_summary_json FROM control_audit)",
            [],
            |row| row.get(0),
        )?;
        assert!(!stored.contains("PRIVATE-MCP-CONTENT"));
        Ok(())
    }

    #[tokio::test]
    async fn image_upload_checks_signature_mime_idempotency_and_private_storage() -> Result<()> {
        assert_eq!(image_mime(b"\x89PNG\r\n\x1a\nsynthetic"), Some("image/png"));
        assert_eq!(image_mime(&[0xff, 0xd8, 0xff, 0]), Some("image/jpeg"));
        assert_eq!(image_mime(b"GIF87asynthetic"), Some("image/gif"));
        assert_eq!(image_mime(b"GIF89asynthetic"), Some("image/gif"));
        assert_eq!(
            image_mime(b"RIFF\x04\x00\x00\x00WEBPsynthetic"),
            Some("image/webp")
        );
        assert_eq!(image_mime(b"<svg/>"), None);

        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let token = URL_SAFE_NO_PAD.encode([22_u8; 32]);
        let app = gateway_test_app(gateway_test_state(database.clone(), &token, None)?);
        let png = b"\x89PNG\r\n\x1a\nsynthetic".to_vec();
        let request = |body: Vec<u8>, content_type: &str, key: &str| {
            Request::builder()
                .method("POST")
                .uri("/v2/uploads/images")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::ORIGIN, "http://127.0.0.1:4765")
                .header("idempotency-key", key)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
        };
        let invalid = app
            .clone()
            .oneshot(request(
                b"<svg/>".to_vec(),
                "image/svg+xml",
                "image-key-0001",
            )?)
            .await?;
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let mismatch = app
            .clone()
            .oneshot(request(png.clone(), "image/jpeg", "image-key-0002")?)
            .await?;
        assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);
        let created = app
            .clone()
            .oneshot(request(png.clone(), "image/png", "image-key-0003")?)
            .await?;
        assert_eq!(created.status(), StatusCode::OK);
        let created = response_json(created).await?;
        let original_upload = created["data"].clone();
        let upload_id = created["data"]["uploadId"]
            .as_str()
            .context("missing uploadId")?;
        let replay = app
            .clone()
            .oneshot(request(png.clone(), "image/png", "image-key-0003")?)
            .await?;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(response_json(replay).await?["data"], original_upload);
        let conflict = app
            .clone()
            .oneshot(request(
                b"\x89PNG\r\n\x1a\ndifferent".to_vec(),
                "image/png",
                "image-key-0003",
            )?)
            .await?;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(conflict).await?["error"]["code"],
            "IDEMPOTENCY_CONFLICT"
        );
        let oversized = app
            .clone()
            .oneshot(request(
                {
                    let mut bytes = vec![0_u8; 20 * 1024 * 1024 + 1];
                    bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
                    bytes
                },
                "image/png",
                "image-key-0004",
            )?)
            .await?;
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let path = database
            .image_staging_dir()
            .join(format!("{upload_id}.bin"));
        assert!(path.is_file());
        #[cfg(unix)]
        {
            assert_eq!(
                std::os::unix::fs::MetadataExt::mode(&fs::metadata(&path)?) & 0o777,
                0o600
            );
            assert_eq!(
                std::os::unix::fs::MetadataExt::mode(&fs::metadata(database.image_staging_dir())?)
                    & 0o777,
                0o700
            );
        }
        fs::write(&path, b"tampered")?;
        let tampered = app
            .clone()
            .oneshot(request(png, "image/png", "image-key-0003")?)
            .await?;
        assert_eq!(tampered.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(tampered).await?["error"]["code"],
            "IMAGE_INVALID"
        );
        #[cfg(unix)]
        {
            fs::remove_file(&path)?;
            let target = temp.path().join("symlink-target");
            fs::write(&target, b"must-not-be-read-as-an-upload")?;
            std::os::unix::fs::symlink(&target, &path)?;
            let symlink = app
                .clone()
                .oneshot(request(
                    b"\x89PNG\r\n\x1a\nsynthetic".to_vec(),
                    "image/png",
                    "image-key-0003",
                )?)
                .await?;
            assert_eq!(symlink.status(), StatusCode::CONFLICT);
            assert_eq!(fs::read(target)?, b"must-not-be-read-as-an-upload");
        }
        let count: i64 =
            database
                .connect()?
                .query_row("SELECT COUNT(*) FROM image_uploads", [], |row| row.get(0))?;
        assert_eq!(count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn image_claim_is_deleted_when_dispatch_cannot_start() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let token = URL_SAFE_NO_PAD.encode([23_u8; 32]);
        let app = gateway_test_app(gateway_test_state(database.clone(), &token, None)?);
        let upload = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v2/uploads/images")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::ORIGIN, "http://127.0.0.1:4765")
                    .header("idempotency-key", "image-cleanup-key")
                    .header(header::CONTENT_TYPE, "image/png")
                    .body(Body::from(b"\x89PNG\r\n\x1a\ncleanup".to_vec()))?,
            )
            .await?;
        let upload = response_json(upload).await?;
        let upload_id = upload["data"]["uploadId"]
            .as_str()
            .context("missing uploadId")?;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v2/commands")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::ORIGIN, "http://127.0.0.1:4765")
                    .header("idempotency-key", "image-command-key")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "capability":"turn.start",
                            "target":{
                                "sourceId":"offline-source",
                                "sourceEpoch":"offline-epoch",
                                "threadKey":"thread-key",
                                "codexThreadId":"thread-id"
                            },
                            "input":{
                                "clientUserMessageId":"client-message",
                                "text":"with image",
                                "uploadIds":[upload_id]
                            }
                        })
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let (state, relative_path): (String, String) = database.connect()?.query_row(
            "SELECT state,relative_path FROM image_uploads WHERE upload_id=?1",
            [upload_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(state, "deleted");
        assert!(!database.image_staging_dir().join(relative_path).exists());
        Ok(())
    }

    #[test]
    fn typed_command_validation_canonicalizes_cwd_and_enforces_slash_contract() -> Result<()> {
        let temp = TempDir::new()?;
        let thread_start = |cwd: &std::path::Path| CreateGatewayCommandRequest {
            capability: "thread.start".into(),
            target: CreateGatewayCommandTarget {
                source_id: "source".into(),
                source_epoch: "epoch".into(),
                thread_key: None,
                codex_thread_id: None,
                expected_turn_id: None,
                expected_request_id: None,
                expected_request_version: None,
            },
            input: json!({"cwd":cwd,"model":null,"personality":"pragmatic","permissions":null}),
        };
        let valid_thread_start = thread_start(temp.path());
        let ControllerOperation::ThreadStart { cwd, .. } =
            controller_operation(&valid_thread_start, Vec::new())?
        else {
            bail!("expected thread start operation");
        };
        assert_eq!(std::path::Path::new(&cwd), fs::canonicalize(temp.path())?);

        let ordinary_file = temp.path().join("not-a-directory");
        fs::write(&ordinary_file, b"synthetic")?;
        for invalid_cwd in [
            std::path::PathBuf::from("relative"),
            temp.path().join("missing-directory"),
            ordinary_file,
        ] {
            assert_eq!(
                controller_operation(&thread_start(&invalid_cwd), Vec::new())
                    .unwrap_err()
                    .code,
                "COMMAND_INVALID"
            );
        }

        let text_request = |text: &str| CreateGatewayCommandRequest {
            capability: "turn.start".into(),
            target: CreateGatewayCommandTarget {
                source_id: "source".into(),
                source_epoch: "epoch".into(),
                thread_key: Some("thread-key".into()),
                codex_thread_id: Some("thread".into()),
                expected_turn_id: None,
                expected_request_id: None,
                expected_request_version: None,
            },
            input: json!({"text":text,"clientUserMessageId":"message"}),
        };
        let error = controller_operation(&text_request("/unknown"), Vec::new()).unwrap_err();
        assert_eq!(error.code, "UNKNOWN_COMMAND");
        let ControllerOperation::TurnStart { text, .. } =
            controller_operation(&text_request("//literal"), Vec::new())?
        else {
            bail!("expected turn start operation");
        };
        assert_eq!(text, "/literal");

        let plan = CreateGatewayCommandRequest {
            capability: "thread.plan".into(),
            target: CreateGatewayCommandTarget {
                source_id: "source".into(),
                source_epoch: "epoch".into(),
                thread_key: Some("thread-key".into()),
                codex_thread_id: Some("thread".into()),
                expected_turn_id: Some("turn".into()),
                expected_request_id: None,
                expected_request_version: None,
            },
            input: json!({
                "prompt":"/literal inside Plan prompt",
                "clientUserMessageId":"plan-message"
            }),
        };
        let ControllerOperation::Plan {
            prompt,
            expected_turn_id,
            ..
        } = controller_operation(&plan, Vec::new())?
        else {
            bail!("expected Plan operation");
        };
        assert_eq!(prompt.as_deref(), Some("/literal inside Plan prompt"));
        assert_eq!(expected_turn_id.as_deref(), Some("turn"));
        let invalid_plan = CreateGatewayCommandRequest {
            input: json!({"prompt":"missing client id","clientUserMessageId":null}),
            ..plan
        };
        assert_eq!(
            controller_operation(&invalid_plan, Vec::new())
                .unwrap_err()
                .code,
            "COMMAND_INVALID"
        );
        Ok(())
    }

    #[tokio::test]
    async fn thread_input_shortcut_uses_same_command_ledger_and_typed_actor() -> Result<()> {
        use crate::controller::{
            ActorRequest, CapabilityCatalog, SourceActorSnapshot, actor_channel,
        };

        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let token = URL_SAFE_NO_PAD.encode([11_u8; 32]);
        let mut state = gateway_test_state(database.clone(), &token, None)?;
        let registry = ControllerRegistry::default();
        let (sender, mut receiver) = actor_channel();
        registry.publish(
            SourceActorSnapshot {
                source_id: "source".into(),
                source_epoch: "epoch".into(),
                state: "ready".into(),
                experimental_api: true,
                catalog: CapabilityCatalog::default(),
                unavailable_reason: None,
            },
            sender,
        );
        state.controller = registry;
        let actor_writer = state.writer.clone();
        let actor = tokio::spawn(async move {
            let Some(ActorRequest::Dispatch { command, reply }) = receiver.recv().await else {
                bail!("missing dispatch request");
            };
            assert!(matches!(
                command.operation,
                ControllerOperation::TurnStart { .. }
            ));
            actor_writer.transition_gateway_command(gateway_transition(
                &command.command_id,
                "dispatching",
                "allow",
                "dispatching",
            ))?;
            actor_writer.transition_gateway_command(gateway_transition(
                &command.command_id,
                "accepted_by_source",
                "allow",
                "accepted_by_source",
            ))?;
            let mut completed =
                gateway_transition(&command.command_id, "completed", "allow", "completed");
            completed.result_summary_json = Some(json!({"turnId":"turn"}).to_string());
            let record = actor_writer.transition_gateway_command(completed)?;
            let _ = reply.send(record);
            Result::<()>::Ok(())
        });
        let app = gateway_test_app(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v2/threads/thread-key/inputs")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header("idempotency-key", "shortcut-key-0001")
                    .header(header::ORIGIN, "http://127.0.0.1:4765")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "sourceId":"source",
                            "sourceEpoch":"epoch",
                            "codexThreadId":"thread",
                            "clientUserMessageId":"client-message",
                            "text":"sensitive shortcut body"
                        })
                        .to_string(),
                    ))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await?;
        assert_eq!(body["data"]["state"], "completed");
        assert_eq!(body["data"]["result"]["turnId"], "turn");
        actor.await??;
        let stored: String = database.connect()?.query_row(
            "SELECT input_summary_json FROM gateway_commands",
            [],
            |row| row.get(0),
        )?;
        assert!(!stored.contains("sensitive shortcut body"));
        Ok(())
    }

    #[tokio::test]
    async fn control_catalog_resolves_thread_without_cross_source_passthrough() -> Result<()> {
        use crate::controller::{
            ActorRequest, CapabilityCatalog, SlashCommandEntry, SourceActorSnapshot,
            SourceControlCatalog, actor_channel,
        };

        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        database.connect()?.execute(
            "INSERT INTO threads(thread_key,store_source_id,codex_thread_id,archived,
               capture_completeness,completeness_reasons_json,projection_json,provenance_json,last_event_seq)
             VALUES ('thread-key','store-source','thread',0,'metadata_only','[]','{}','{}',0)",
            [],
        )?;
        let token = URL_SAFE_NO_PAD.encode([12_u8; 32]);
        let mut state = gateway_test_state(database, &token, None)?;
        let registry = ControllerRegistry::default();
        let (sender, mut receiver) = actor_channel();
        registry.publish(
            SourceActorSnapshot {
                source_id: "source".into(),
                source_epoch: "epoch".into(),
                state: "ready".into(),
                experimental_api: true,
                catalog: CapabilityCatalog::default(),
                unavailable_reason: None,
            },
            sender,
        );
        state.controller = registry;
        let actor = tokio::spawn(async move {
            let Some(ActorRequest::Catalog {
                thread_id,
                thread_key,
                reply,
            }) = receiver.recv().await
            else {
                bail!("missing catalog request");
            };
            assert_eq!(thread_id.as_deref(), Some("thread"));
            assert_eq!(thread_key.as_deref(), Some("thread-key"));
            let _ = reply.send(SourceControlCatalog {
                source_id: "source".into(),
                source_epoch: "epoch".into(),
                thread_loaded: true,
                active_turn_id: None,
                collaboration_mode: None,
                goal: None,
                capabilities: CapabilityCatalog::default(),
                slash_commands: vec![SlashCommandEntry {
                    name: "/model".into(),
                    capability: "thread.settings.model".into(),
                    interaction_required_without_argument: true,
                }],
            });
            Result::<()>::Ok(())
        });
        let response = gateway_test_app(state)
            .oneshot(
                Request::builder()
                    .uri("/v2/control/catalog?sourceId=source&threadKey=thread-key")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await?;
        assert_eq!(body["data"]["sourceEpoch"], "epoch");
        assert_eq!(body["data"]["slashCommands"][0]["name"], "/model");
        actor.await??;
        Ok(())
    }

    #[tokio::test]
    async fn slash_picker_is_structured_and_unknown_command_is_never_model_input() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let token = URL_SAFE_NO_PAD.encode([13_u8; 32]);
        let app = gateway_test_app(gateway_test_state(database.clone(), &token, None)?);
        let request = |key: &str, text: &str| -> Result<Request> {
            Ok(Request::builder()
                .method("POST")
                .uri("/v2/threads/thread-key/inputs")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("idempotency-key", key)
                .header(header::ORIGIN, "http://127.0.0.1:4765")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "sourceId":"source",
                        "sourceEpoch":"epoch",
                        "codexThreadId":"thread",
                        "clientUserMessageId":"message",
                        "text":text
                    })
                    .to_string(),
                ))?)
        };
        let picker = app
            .clone()
            .oneshot(request("picker-key-0001", "/model")?)
            .await?;
        assert_eq!(picker.status(), StatusCode::CONFLICT);
        let picker = response_json(picker).await?;
        assert_eq!(picker["error"]["code"], "INTERACTION_REQUIRED");
        assert_eq!(
            picker["error"]["details"]["interaction"]["type"],
            "modelPicker"
        );
        assert_eq!(
            database
                .connect()?
                .query_row("SELECT COUNT(*) FROM gateway_commands", [], |row| row
                    .get::<_, i64>(0),)?,
            0
        );

        let unknown = app
            .oneshot(request("unknown-key-0001", "/does-not-exist")?)
            .await?;
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(unknown).await?["error"]["code"],
            "UNKNOWN_COMMAND"
        );
        let stored: (String, String) = database.connect()?.query_row(
            "SELECT state,input_summary_json FROM gateway_commands",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(stored.0, "rejected");
        assert!(!stored.1.contains("does-not-exist"));
        Ok(())
    }

    #[tokio::test]
    async fn status_mcp_and_usage_slash_results_are_local_gateway_cards() -> Result<()> {
        use crate::controller::{
            ActorRequest, CapabilityCatalog, SourceActorSnapshot, SourceControlCatalog,
            actor_channel,
        };

        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let token = URL_SAFE_NO_PAD.encode([14_u8; 32]);
        let mut state = gateway_test_state(database.clone(), &token, None)?;
        let registry = ControllerRegistry::default();
        let (sender, mut receiver) = actor_channel();
        registry.publish(
            SourceActorSnapshot {
                source_id: "source".into(),
                source_epoch: "epoch".into(),
                state: "ready".into(),
                experimental_api: true,
                catalog: CapabilityCatalog::default(),
                unavailable_reason: None,
            },
            sender,
        );
        state.controller = registry;
        let actor = tokio::spawn(async move {
            let mut capabilities = CapabilityCatalog::default();
            for (method, result) in [
                (
                    "mcpServerStatus/list",
                    json!({"data":[{"name":"synthetic","status":"ready"}]}),
                ),
                (
                    "account/usage/read",
                    json!({"summary":{"lifetimeTokens":10}}),
                ),
                (
                    "account/rateLimits/read",
                    json!({"rateLimits":{"primary":{"usedPercent":5}}}),
                ),
            ] {
                capabilities.record_response(method, false, &json!({"result":result}));
            }
            for _ in 0..3 {
                let Some(ActorRequest::Catalog { reply, .. }) = receiver.recv().await else {
                    bail!("missing local card catalog request");
                };
                let _ = reply.send(SourceControlCatalog {
                    source_id: "source".into(),
                    source_epoch: "epoch".into(),
                    thread_loaded: true,
                    active_turn_id: Some("turn".into()),
                    collaboration_mode: Some(json!({"mode":"plan"})),
                    goal: Some(json!({"status":"active","objective":"synthetic"})),
                    capabilities: capabilities.clone(),
                    slash_commands: Vec::new(),
                });
            }
            Result::<()>::Ok(())
        });
        let app = gateway_test_app(state);
        for (index, (text_input, expected_type)) in [
            ("/status", "status"),
            ("/mcp verbose", "mcp"),
            ("/usage", "usage"),
        ]
        .into_iter()
        .enumerate()
        {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v2/threads/thread-key/inputs")
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .header("idempotency-key", format!("local-card-key-{index:04}"))
                        .header(header::ORIGIN, "http://127.0.0.1:4765")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            json!({
                                "sourceId":"source",
                                "sourceEpoch":"epoch",
                                "codexThreadId":"thread",
                                "expectedTurnId":"turn",
                                "clientUserMessageId":format!("local-card-message-{index}"),
                                "text":text_input,
                            })
                            .to_string(),
                        ))?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::OK);
            let body = response_json(response).await?;
            assert_eq!(body["data"]["kind"], "gatewayStatusCard");
            assert_eq!(body["data"]["cardType"], expected_type);
        }
        actor.await??;
        assert_eq!(
            database
                .connect()?
                .query_row("SELECT COUNT(*) FROM gateway_commands", [], |row| row
                    .get::<_, i64>(0))?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn command_api_accepts_only_verified_tailscale_principal() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let access = ServeAccess {
            authority: "observer.example.ts.net".into(),
            origin: "https://observer.example.ts.net".into(),
            viewer_url: "https://observer.example.ts.net/".into(),
        };
        let token = URL_SAFE_NO_PAD.encode([5_u8; 32]);
        let app = gateway_test_app(gateway_test_state(database.clone(), &token, Some(access))?);
        let mut request = Request::builder()
            .method("POST")
            .uri("/v2/commands")
            .header(header::HOST, "observer.example.ts.net")
            .header("x-forwarded-proto", "https")
            .header("tailscale-user-login", "user@example.com")
            .header("idempotency-key", "tailscale-key-0001")
            .header(header::ORIGIN, "https://observer.example.ts.net")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "capability":"turn.start",
                    "target":{"sourceId":"source","sourceEpoch":"epoch","threadKey":"thread-key","codexThreadId":"thread"},
                    "input":{"text":"fixture","clientUserMessageId":"tailscale-message"}
                })
                .to_string(),
            ))?;
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:50000".parse::<SocketAddr>()?));
        let response = app.oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let principal: String = database.connect()?.query_row(
            "SELECT principal_id FROM gateway_commands",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(principal, "tailscale:user@example.com");
        Ok(())
    }

    #[test]
    fn tailscale_auth_requires_loopback_proxy_https_host_and_identity() -> Result<()> {
        let access = ServeAccess {
            authority: "observer.example.ts.net".into(),
            origin: "https://observer.example.ts.net".into(),
            viewer_url: "https://observer.example.ts.net/".into(),
        };
        let request = |peer: &str, host: &str, login: Option<&str>| -> Result<Request> {
            let mut builder = Request::builder()
                .header(header::HOST, host)
                .header("x-forwarded-proto", "https");
            if let Some(login) = login {
                builder = builder.header("tailscale-user-login", login);
            }
            let mut request = builder.body(Body::empty())?;
            request
                .extensions_mut()
                .insert(ConnectInfo(peer.parse::<SocketAddr>()?));
            Ok(request)
        };

        assert_eq!(
            tailscale_request_principal(
                &access,
                &request(
                    "127.0.0.1:50000",
                    "observer.example.ts.net",
                    Some("user@example.com")
                )?
            )
            .as_deref(),
            Some("tailscale:user@example.com")
        );
        assert!(
            tailscale_request_principal(
                &access,
                &request(
                    "100.64.0.1:50000",
                    "observer.example.ts.net",
                    Some("user@example.com")
                )?
            )
            .is_none()
        );
        assert!(
            tailscale_request_principal(
                &access,
                &request(
                    "127.0.0.1:50000",
                    "wrong.example.ts.net",
                    Some("user@example.com")
                )?
            )
            .is_none()
        );
        assert!(
            tailscale_request_principal(
                &access,
                &request("127.0.0.1:50000", "observer.example.ts.net", None)?
            )
            .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn errors_use_stable_non_leaking_contract() -> Result<()> {
        let response = api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "OBSERVER_BUSY",
            "retry later",
        );
        let body: Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await?)?;
        assert_eq!(body["apiVersion"], "v1");
        assert_eq!(body["error"]["code"], "OBSERVER_BUSY");
        assert_eq!(body["error"]["retryable"], true);
        assert!(
            body["error"]["requestId"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert_eq!(body["error"]["details"], json!({}));
        Ok(())
    }

    #[test]
    fn cursor_round_trip_and_tamper_detection() -> Result<()> {
        let token = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let cursor = ThreadCursor {
            endpoint: "threads".into(),
            query_fingerprint: "query".into(),
            as_of_event_seq: 42,
            last_recency_at_ms: 10,
            last_thread_key: "thread".into(),
        };
        let encoded = encode_cursor(&cursor, &token)?;
        let decoded = decode_cursor(&encoded, &token)?;
        assert_eq!(decoded.as_of_event_seq, 42);
        assert_eq!(decoded.last_thread_key, "thread");

        let mut tampered = encoded.into_bytes();
        tampered[0] = if tampered[0] == b'A' { b'B' } else { b'A' };
        assert!(decode_cursor(std::str::from_utf8(&tampered)?, &token).is_err());

        let request = RequestKey {
            source_id: "source".into(),
            source_epoch: "epoch".into(),
            request_id: "request".into(),
        };
        let encoded = encode_request_key(&request, &token)?;
        assert_eq!(decode_request_key(&encoded, &token)?, request);
        let stream = V2StreamCursor {
            event_seq: 12,
            command_transition_seq: 34,
        };
        let encoded = encode_v2_stream_cursor(&stream, &token)?;
        assert_eq!(decode_v2_stream_cursor(&encoded, &token)?, stream);
        Ok(())
    }

    #[tokio::test]
    async fn v2_stream_starts_at_retention_floor_and_replays_from_composite_cursor() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let connection = database.connect()?;
        connection.execute(
            "INSERT INTO sources(source_id,kind,stable_identity,config_json,status,created_at_ms,updated_at_ms)
             VALUES ('source','app_server','source','{}','ready',0,0)",
            [],
        )?;
        connection.execute(
            "INSERT INTO source_epochs(source_id,epoch_id,opened_at_ms) VALUES ('source','epoch',0)",
            [],
        )?;
        for sequence in 1..=6_i64 {
            connection.execute(
                "INSERT INTO raw_events(event_id,source_id,epoch_id,source_seq,dedupe_key,observed_at_ms,
                   thread_key,codex_thread_id,method,phase,durability,source_fingerprint,stored_raw_hash,
                   raw_json,redaction_json,decode_status,store_source_id)
                 VALUES (?1,'source','epoch',?2,?1,?2,'thread','thread',?3,'completed','live',?1,?1,'{}','{}','decoded','source')",
                rusqlite::params![format!("event-{sequence}"), sequence, format!("event/{sequence}")],
            )?;
        }
        connection.execute(
            "UPDATE retention_state SET value_integer=5 WHERE key='raw_low_watermark'",
            [],
        )?;
        drop(connection);
        let token = URL_SAFE_NO_PAD.encode([24_u8; 32]);
        let app = gateway_test_app(gateway_test_state(database.clone(), &token, None)?);
        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v2/stream")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(first.status(), StatusCode::OK);
        let first = first_sse_frame(first).await?;
        assert!(first.contains("\"eventSeq\":6"));
        let cursor = first
            .lines()
            .find_map(|line| line.strip_prefix("id:"))
            .map(str::trim)
            .context("SSE event did not contain a cursor")?;
        assert_eq!(decode_v2_stream_cursor(cursor, &token)?.event_seq, 6);

        database.connect()?.execute(
            "INSERT INTO raw_events(event_id,source_id,epoch_id,source_seq,dedupe_key,observed_at_ms,
               thread_key,codex_thread_id,method,phase,durability,source_fingerprint,stored_raw_hash,
               raw_json,redaction_json,decode_status,store_source_id)
             VALUES ('event-7','source','epoch',7,'event-7',7,'thread','thread','event/7','completed','live',
               'event-7','event-7','{}','{}','decoded','source')",
            [],
        )?;
        let replay = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v2/stream?cursor={cursor}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(replay.status(), StatusCode::OK);
        let replay = first_sse_frame(replay).await?;
        assert!(replay.contains("\"eventSeq\":7"));
        assert!(!replay.contains("\"eventSeq\":6"));
        Ok(())
    }

    #[tokio::test]
    async fn thread_cursor_pages_without_duplicates_and_binds_filters() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let connection = database.connect()?;
        for index in 1..=3_i64 {
            connection.execute(
                "INSERT INTO threads(thread_key,store_source_id,codex_thread_id,archived,
                   capture_completeness,completeness_reasons_json,created_at_ms,updated_at_ms,
                   recency_at_ms,projection_json,provenance_json,last_event_seq)
                 VALUES (?1,'source',?1,0,'durable_complete','[]',?2,?2,?2,'{}','{}',?3)",
                rusqlite::params![format!("thread-{index}"), index * 1000, 0],
            )?;
        }
        let state = ApiState {
            writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
            controller: ControllerRegistry::default(),
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([9_u8; 32])),
            fingerprint_key: [7; 32],
            strict_origin: true,
            allowed_origins: Arc::new(Vec::new()),
            live_modes: Arc::new(Vec::new()),
            blob_downloads: Arc::new(Semaphore::new(1)),
            settings: Arc::new(json!({})),
            tailscale: None,
        };
        let first = query_threads(
            &state,
            ThreadQuery {
                limit: Some(2),
                ..ThreadQuery::default()
            },
        )
        .map_err(cursor_test_error)?;
        let first: Value =
            serde_json::from_slice(&axum::body::to_bytes(first.into_body(), usize::MAX).await?)?;
        assert_eq!(first["data"].as_array().unwrap().len(), 2);
        let cursor = first["nextCursor"].as_str().unwrap().to_string();

        let second = query_threads(
            &state,
            ThreadQuery {
                cursor: Some(cursor.clone()),
                limit: Some(2),
                ..ThreadQuery::default()
            },
        )
        .map_err(cursor_test_error)?;
        let second: Value =
            serde_json::from_slice(&axum::body::to_bytes(second.into_body(), usize::MAX).await?)?;
        assert_eq!(second["data"].as_array().unwrap().len(), 1);
        assert_eq!(first["asOfEventSeq"], second["asOfEventSeq"]);
        assert_ne!(
            first["data"][1]["threadKey"],
            second["data"][0]["threadKey"]
        );

        let mismatched = query_threads(
            &state,
            ThreadQuery {
                cursor: Some(cursor),
                archived: Some(false),
                ..ThreadQuery::default()
            },
        );
        assert!(matches!(mismatched, Err(CursorFailure::Invalid(_))));
        Ok(())
    }

    #[tokio::test]
    async fn ten_thousand_thread_and_search_queries_remain_bounded() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let mut connection = database.connect()?;
        let transaction = connection.transaction()?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO threads(thread_key,store_source_id,codex_thread_id,archived,
                   capture_completeness,completeness_reasons_json,recency_at_ms,
                   projection_json,provenance_json,last_event_seq)
                 VALUES (?1,'scale-source',?1,0,'durable_complete','[]',?2,'{}','{}',0)",
            )?;
            for index in 0..10_000_i64 {
                statement.execute(rusqlite::params![format!("thread-{index:05}"), index])?;
            }
        }
        {
            let mut item_statement = transaction.prepare(
                "INSERT INTO items(thread_key,turn_scope,item_id,turn_id,item_type,status,started_at_ms,
                   summary_text,projection_json,provenance_json,last_event_seq)
                 VALUES ('thread-00000','turn-scale',?1,'turn-scale','message','completed',?2,?3,'{}','{}',0)",
            )?;
            let mut search_statement = transaction.prepare(
                "INSERT INTO search_index(entity_key,thread_key,item_id,text)
                 VALUES (?1,'thread-00000',?2,?3)",
            )?;
            for index in 0..10_000_i64 {
                let item_id = format!("item-{index:05}");
                let summary = format!("needle scale item {index}");
                item_statement.execute(rusqlite::params![item_id, index, summary])?;
                search_statement.execute(rusqlite::params![
                    format!("thread-00000:turn-scale:item-{index:05}"),
                    item_id,
                    summary
                ])?;
            }
        }
        transaction.commit()?;
        let state = ApiState {
            writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
            controller: ControllerRegistry::default(),
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([6_u8; 32])),
            fingerprint_key: [7; 32],
            strict_origin: true,
            allowed_origins: Arc::new(Vec::new()),
            live_modes: Arc::new(Vec::new()),
            blob_downloads: Arc::new(Semaphore::new(1)),
            settings: Arc::new(json!({})),
            tailscale: None,
        };

        let started = std::time::Instant::now();
        let response = query_threads(
            &state,
            ThreadQuery {
                limit: Some(50),
                ..ThreadQuery::default()
            },
        )
        .map_err(cursor_test_error)?;
        let elapsed = started.elapsed();
        let response: Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await?)?;
        assert_eq!(response["data"].as_array().unwrap().len(), 50);
        assert!(response["nextCursor"].is_string());
        assert!(
            elapsed < Duration::from_secs(5),
            "thread query took {elapsed:?}"
        );

        let started = std::time::Instant::now();
        let response = query_search(
            &state,
            SearchQuery {
                q: "needle".into(),
                cursor: None,
                limit: Some(50),
            },
        )
        .map_err(cursor_test_error)?;
        let elapsed = started.elapsed();
        let response: Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await?)?;
        assert_eq!(response["data"].as_array().unwrap().len(), 50);
        assert!(response["nextCursor"].is_string());
        assert!(
            elapsed < Duration::from_secs(5),
            "search query took {elapsed:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn turn_item_and_search_cursors_page_and_bind_queries() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let connection = database.connect()?;
        connection.execute_batch(
            "INSERT INTO threads(thread_key,store_source_id,codex_thread_id,archived,
               capture_completeness,completeness_reasons_json,projection_json,provenance_json,last_event_seq)
             VALUES ('thread-a','source','thread-a',0,'durable_complete','[]','{}','{}',0),
                    ('thread-b','source','thread-b',0,'durable_complete','[]','{}','{}',0);
             INSERT INTO turns(thread_key,turn_id,status,capture_completeness,completeness_reasons_json,
               coverage_json,started_at_ms,projection_json,provenance_json,last_event_seq)
             VALUES ('thread-a','turn-1','completed','durable_complete','[]','{}',1000,'{}','{}',0),
                    ('thread-a','turn-2','completed','durable_complete','[]','{}',1000,'{}','{}',0),
                    ('thread-a','turn-3','completed','durable_complete','[]','{}',2000,'{}','{}',0);
             INSERT INTO items(thread_key,turn_scope,item_id,turn_id,item_type,status,started_at_ms,
               summary_text,projection_json,provenance_json,last_event_seq)
             VALUES ('thread-a','turn-1','item-1','turn-1','message','completed',1000,'needle alpha','{}','{}',0),
                    ('thread-a','turn-1','item-2','turn-1','message','completed',1000,'needle beta','{}','{}',0),
                    ('thread-a','turn-2','item-3','turn-2','message','completed',2000,'needle gamma','{}','{}',0);
             INSERT INTO search_index(entity_key,thread_key,item_id,text)
             VALUES ('thread-a:turn-1:item-1','thread-a','item-1','needle alpha'),
                    ('thread-a:turn-1:item-2','thread-a','item-2','needle beta'),
                    ('thread-a:turn-2:item-3','thread-a','item-3','needle gamma');",
        )?;
        drop(connection);
        let state = ApiState {
            writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
            controller: ControllerRegistry::default(),
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([8_u8; 32])),
            fingerprint_key: [7; 32],
            strict_origin: true,
            allowed_origins: Arc::new(Vec::new()),
            live_modes: Arc::new(Vec::new()),
            blob_downloads: Arc::new(Semaphore::new(1)),
            settings: Arc::new(json!({})),
            tailscale: None,
        };

        let first = query_turns(
            &state,
            "thread-a",
            TurnQuery {
                limit: Some(2),
                ..TurnQuery::default()
            },
        )
        .map_err(cursor_test_error)?;
        let first: Value =
            serde_json::from_slice(&axum::body::to_bytes(first.into_body(), usize::MAX).await?)?;
        let turn_cursor = first["nextCursor"].as_str().unwrap().to_string();
        let second = query_turns(
            &state,
            "thread-a",
            TurnQuery {
                cursor: Some(turn_cursor.clone()),
                limit: Some(2),
            },
        )
        .map_err(cursor_test_error)?;
        let second: Value =
            serde_json::from_slice(&axum::body::to_bytes(second.into_body(), usize::MAX).await?)?;
        assert_eq!(first["data"].as_array().unwrap().len(), 2);
        assert_eq!(second["data"].as_array().unwrap().len(), 1);
        assert_ne!(first["data"][1]["turnId"], second["data"][0]["turnId"]);
        assert!(matches!(
            query_turns(
                &state,
                "thread-b",
                TurnQuery {
                    cursor: Some(turn_cursor),
                    limit: None
                }
            ),
            Err(CursorFailure::Invalid(_))
        ));

        let first = query_items(
            &state,
            "thread-a",
            ItemQuery {
                limit: Some(2),
                ..ItemQuery::default()
            },
        )
        .map_err(cursor_test_error)?;
        let first: Value =
            serde_json::from_slice(&axum::body::to_bytes(first.into_body(), usize::MAX).await?)?;
        let item_cursor = first["nextCursor"].as_str().unwrap().to_string();
        let second = query_items(
            &state,
            "thread-a",
            ItemQuery {
                cursor: Some(item_cursor.clone()),
                limit: Some(2),
                ..ItemQuery::default()
            },
        )
        .map_err(cursor_test_error)?;
        let second: Value =
            serde_json::from_slice(&axum::body::to_bytes(second.into_body(), usize::MAX).await?)?;
        assert_eq!(second["data"].as_array().unwrap().len(), 1);
        assert_ne!(first["data"][1]["itemId"], second["data"][0]["itemId"]);
        assert!(matches!(
            query_items(
                &state,
                "thread-a",
                ItemQuery {
                    cursor: Some(item_cursor),
                    item_type: Some("tool_call".into()),
                    ..ItemQuery::default()
                }
            ),
            Err(CursorFailure::Invalid(_))
        ));

        let first = query_search(
            &state,
            SearchQuery {
                q: "needle".into(),
                cursor: None,
                limit: Some(2),
            },
        )
        .map_err(cursor_test_error)?;
        let first: Value =
            serde_json::from_slice(&axum::body::to_bytes(first.into_body(), usize::MAX).await?)?;
        let search_cursor = first["nextCursor"].as_str().unwrap().to_string();
        let second = query_search(
            &state,
            SearchQuery {
                q: "needle".into(),
                cursor: Some(search_cursor.clone()),
                limit: Some(2),
            },
        )
        .map_err(cursor_test_error)?;
        let second: Value =
            serde_json::from_slice(&axum::body::to_bytes(second.into_body(), usize::MAX).await?)?;
        assert_eq!(first["data"].as_array().unwrap().len(), 2);
        assert_eq!(second["data"].as_array().unwrap().len(), 1);
        assert_ne!(
            first["data"][1]["entityKey"],
            second["data"][0]["entityKey"]
        );
        assert!(matches!(
            query_search(
                &state,
                SearchQuery {
                    q: "different".into(),
                    cursor: Some(search_cursor),
                    limit: None
                }
            ),
            Err(CursorFailure::Invalid(_))
        ));
        Ok(())
    }

    #[test]
    fn expired_event_cursor_returns_gone() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        database.connect()?.execute(
            "UPDATE retention_state SET value_integer=5 WHERE key='raw_low_watermark'",
            [],
        )?;
        let state = ApiState {
            writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
            controller: ControllerRegistry::default(),
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([9_u8; 32])),
            fingerprint_key: [7; 32],
            strict_origin: true,
            allowed_origins: Arc::new(Vec::new()),
            live_modes: Arc::new(Vec::new()),
            blob_downloads: Arc::new(Semaphore::new(1)),
            settings: Arc::new(json!({})),
            tailscale: None,
        };
        let response = event_query(&state, 4, Some(10), None, None, None);
        assert_eq!(response.status(), StatusCode::GONE);
        Ok(())
    }

    #[tokio::test]
    async fn snapshot_watermark_resumes_events_without_gap_or_duplicate() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let connection = database.connect()?;
        connection.execute(
            "INSERT INTO sources(source_id,kind,stable_identity,config_json,status,created_at_ms,updated_at_ms)
             VALUES ('source','rollout','source','{}','online',0,0)",
            [],
        )?;
        connection.execute(
            "INSERT INTO source_epochs(source_id,epoch_id,opened_at_ms) VALUES ('source','epoch',0)",
            [],
        )?;
        for sequence in 1..=2_i64 {
            connection.execute(
                "INSERT INTO raw_events(event_id,source_id,epoch_id,source_seq,dedupe_key,observed_at_ms,
                   thread_key,codex_thread_id,method,phase,durability,source_fingerprint,stored_raw_hash,
                   raw_json,redaction_json,decode_status,store_source_id)
                 VALUES (?1,'source','epoch',?2,?1,?2,'thread','thread',?3,'completed','durable',?1,?1,'{}','{}','decoded','source')",
                rusqlite::params![format!("event-{sequence}"), sequence, format!("event/{sequence}")],
            )?;
        }
        connection.execute(
            "INSERT INTO threads(thread_key,store_source_id,codex_thread_id,archived,capture_completeness,
               completeness_reasons_json,recency_at_ms,projection_json,provenance_json,last_event_seq)
             VALUES ('thread','source','thread',0,'durable_complete','[]',2,'{}','{}',2)",
            [],
        )?;
        let state = ApiState {
            writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
            controller: ControllerRegistry::default(),
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([5_u8; 32])),
            fingerprint_key: [7; 32],
            strict_origin: true,
            allowed_origins: Arc::new(Vec::new()),
            live_modes: Arc::new(Vec::new()),
            blob_downloads: Arc::new(Semaphore::new(1)),
            settings: Arc::new(json!({})),
            tailscale: None,
        };
        let snapshot = query_threads(&state, ThreadQuery::default()).map_err(cursor_test_error)?;
        let snapshot: Value =
            serde_json::from_slice(&axum::body::to_bytes(snapshot.into_body(), usize::MAX).await?)?;
        assert_eq!(snapshot["asOfEventSeq"], 2);

        state.database.connect()?.execute(
            "INSERT INTO raw_events(event_id,source_id,epoch_id,source_seq,dedupe_key,observed_at_ms,
               thread_key,codex_thread_id,method,phase,durability,source_fingerprint,stored_raw_hash,
               raw_json,redaction_json,decode_status,store_source_id)
             VALUES ('event-3','source','epoch',3,'event-3',3,'thread','thread','event/3','completed',
               'durable','event-3','event-3','{}','{}','decoded','source')",
            [],
        )?;
        let replay = event_query(&state, 2, Some(200), None, None, None);
        let replay: Value =
            serde_json::from_slice(&axum::body::to_bytes(replay.into_body(), usize::MAX).await?)?;
        assert_eq!(replay["data"].as_array().unwrap().len(), 1);
        assert_eq!(replay["data"][0]["eventSeq"], 3);
        Ok(())
    }

    #[test]
    fn byte_ranges_support_prefix_open_and_suffix_forms() {
        assert_eq!(parse_byte_range(None, 10), Ok(None));
        assert_eq!(parse_byte_range(Some("bytes=2-5"), 10), Ok(Some((2, 5))));
        assert_eq!(parse_byte_range(Some("bytes=7-"), 10), Ok(Some((7, 9))));
        assert_eq!(parse_byte_range(Some("bytes=-3"), 10), Ok(Some((7, 9))));
        assert!(parse_byte_range(Some("bytes=10-"), 10).is_err());
        assert!(parse_byte_range(Some("bytes=1-2,4-5"), 10).is_err());
    }

    #[test]
    fn websocket_subscribe_filters_are_bounded_and_match_events() -> Result<()> {
        let request: WsSubscribe = serde_json::from_value(json!({
            "type":"subscribe",
            "afterEventSeq":12,
            "filters":{"threadKeys":["thread-a"],"sourceIds":["source-a"],"methods":["item/completed"]}
        }))?;
        assert!(request.filters.is_valid());
        assert!(request.filters.matches(&json!({
            "threadKey":"thread-a","sourceId":"source-a","method":"item/completed"
        })));
        assert!(!request.filters.matches(&json!({
            "threadKey":"thread-b","sourceId":"source-a","method":"item/completed"
        })));
        let invalid = StreamFilters {
            methods: vec!["x".repeat(257)],
            ..StreamFilters::default()
        };
        assert!(!invalid.is_valid());
        Ok(())
    }

    #[tokio::test]
    async fn sse_rejects_invalid_last_event_id() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Arc::new(Database::open(&temp.path().join("observer.sqlite"))?);
        database.migrate()?;
        let state = ApiState {
            writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
            controller: ControllerRegistry::default(),
            database,
            token: Arc::new("token".into()),
            fingerprint_key: [7; 32],
            strict_origin: true,
            allowed_origins: Arc::new(Vec::new()),
            live_modes: Arc::new(Vec::new()),
            blob_downloads: Arc::new(Semaphore::new(1)),
            settings: Arc::new(json!({})),
            tailscale: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", HeaderValue::from_static("not-an-integer"));
        let response = sse_stream(
            State(state),
            headers,
            Query(EventQuery {
                after_event_seq: None,
                limit: None,
                thread_key: None,
                source_id: None,
                method: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        Ok(())
    }

    #[tokio::test]
    async fn blob_download_returns_safe_range_response() -> Result<()> {
        let temp = TempDir::new()?;
        let blob_dir = temp.path().join("blobs");
        let database = Arc::new(Database::open_with_blobs(
            &temp.path().join("observer.sqlite"),
            &blob_dir,
            1024,
        )?);
        database.migrate()?;
        std::fs::create_dir_all(blob_dir.join("ab"))?;
        std::fs::write(blob_dir.join("ab/blob.json"), b"0123456789")?;
        let connection = database.connect()?;
        connection.execute(
            "INSERT INTO blobs(blob_id,stored_hash,media_type,size_bytes,relative_path,created_event_seq,created_at_ms)
             VALUES ('blob_test','hash','application/json',10,'ab/blob.json',1,0)",
            [],
        )?;
        connection.execute(
            "INSERT INTO blob_references(reference_kind,reference_key,blob_id,event_seq)
             VALUES ('projection','item:test','blob_test',1)",
            [],
        )?;
        drop(connection);
        let state = ApiState {
            writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
            controller: ControllerRegistry::default(),
            database,
            token: Arc::new("token".into()),
            fingerprint_key: [7; 32],
            strict_origin: true,
            allowed_origins: Arc::new(Vec::new()),
            live_modes: Arc::new(Vec::new()),
            blob_downloads: Arc::new(Semaphore::new(1)),
            settings: Arc::new(json!({})),
            tailscale: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_static("bytes=2-5"));
        let response = blob(State(state), Path("blob_test".into()), headers).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(
            response.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=\"observer-blob.json\""
        );
        let body = axum::body::to_bytes(response.into_body(), 16).await?;
        assert_eq!(&body[..], b"2345");
        Ok(())
    }

    #[tokio::test]
    async fn thread_detail_resolves_parent_fork_children_and_execution_metadata() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = Config::default();
        config.sources[0].codex_home =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex-home");
        config.storage.database = temp.path().join("observer.sqlite");
        config.storage.fingerprint_key_file = temp.path().join("fingerprint.key");
        let database = Arc::new(Database::open(&config.storage.database)?);
        database.migrate()?;
        Importer::new(&config, database.as_ref())?.import_all()?;
        let connection = database.connect()?;
        let child_key: String = connection.query_row(
            "SELECT thread_key FROM threads WHERE codex_thread_id='00000000-0000-7000-8000-000000000002'",
            [],
            |row| row.get(0),
        )?;
        let parent_key: String = connection.query_row(
            "SELECT thread_key FROM threads WHERE codex_thread_id='00000000-0000-7000-8000-000000000001'",
            [],
            |row| row.get(0),
        )?;
        drop(connection);
        let state = ApiState {
            writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
            controller: ControllerRegistry::default(),
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([9_u8; 32])),
            fingerprint_key: [7; 32],
            strict_origin: true,
            allowed_origins: Arc::new(Vec::new()),
            live_modes: Arc::new(Vec::new()),
            blob_downloads: Arc::new(Semaphore::new(1)),
            settings: Arc::new(json!({})),
            tailscale: None,
        };
        let child = thread_detail(State(state.clone()), Path(child_key)).await;
        let child: Value =
            serde_json::from_slice(&axum::body::to_bytes(child.into_body(), usize::MAX).await?)?;
        assert_eq!(child["data"]["relations"]["parent"]["resolved"], true);
        assert_eq!(child["data"]["relations"]["forkedFrom"]["resolved"], true);
        assert_eq!(child["data"]["thread"]["reasoningEffort"], "high");
        assert_eq!(child["data"]["thread"]["modelProvider"], "openai");

        let parent = thread_detail(State(state), Path(parent_key)).await;
        let parent: Value =
            serde_json::from_slice(&axum::body::to_bytes(parent.into_body(), usize::MAX).await?)?;
        assert_eq!(
            parent["data"]["relations"]["children"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(parent["data"]["coverageSummary"]["turns"][0]["count"], 1);
        assert_eq!(
            parent["data"]["thread"]["project"]["key"],
            "/demo/local-observer"
        );
        assert_eq!(
            parent["data"]["thread"]["project"]["name"],
            "local-observer"
        );
        assert_eq!(
            parent["data"]["thread"]["context"]["session"]["baseInstructions"]["text"],
            "# Project AGENTS.md\n\nKeep changes read-only."
        );
        Ok(())
    }

    #[tokio::test]
    async fn projects_group_threads_by_cwd_and_context_is_exposed() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = Config::default();
        config.sources[0].codex_home =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex-home");
        config.storage.database = temp.path().join("observer.sqlite");
        config.storage.fingerprint_key_file = temp.path().join("fingerprint.key");
        let database = Arc::new(Database::open(&config.storage.database)?);
        database.migrate()?;
        Importer::new(&config, database.as_ref())?.import_all()?;
        let state = ApiState {
            writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
            controller: ControllerRegistry::default(),
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([9_u8; 32])),
            fingerprint_key: [7; 32],
            strict_origin: true,
            allowed_origins: Arc::new(Vec::new()),
            live_modes: Arc::new(Vec::new()),
            blob_downloads: Arc::new(Semaphore::new(1)),
            settings: Arc::new(json!({})),
            tailscale: None,
        };
        let response = projects(State(state.clone())).await;
        let body: Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await?)?;
        let project_rows = body["data"].as_array().unwrap();
        assert_eq!(project_rows.len(), 2);
        let local = project_rows
            .iter()
            .find(|project| project["project"]["key"] == "/demo/local-observer")
            .unwrap();
        assert_eq!(local["threadCount"], 2);
        assert_eq!(local["currentThreadCount"], 2);

        let filtered = query_threads(
            &state,
            ThreadQuery {
                project: Some("/demo/local-observer".into()),
                ..ThreadQuery::default()
            },
        )
        .map_err(cursor_test_error)?;
        let filtered: Value =
            serde_json::from_slice(&axum::body::to_bytes(filtered.into_body(), usize::MAX).await?)?;
        assert_eq!(filtered["data"].as_array().unwrap().len(), 2);
        assert!(
            filtered["data"]
                .as_array()
                .unwrap()
                .iter()
                .all(|thread| thread["project"]["key"] == "/demo/local-observer")
        );

        state.database.connect()?.execute(
            "UPDATE threads SET cwd='/Users/demo/Documents/Codex/2026-08-28/generated-name',
             originator='Codex Desktop',project_key=NULL
             WHERE codex_thread_id='00000000-0000-7000-8000-000000000003'",
            [],
        )?;
        let response = projects(State(state.clone())).await;
        let body: Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await?)?;
        assert_eq!(body["data"].as_array().unwrap().len(), 1);

        let ungrouped = query_threads(&state, ThreadQuery::default()).map_err(cursor_test_error)?;
        let ungrouped: Value = serde_json::from_slice(
            &axum::body::to_bytes(ungrouped.into_body(), usize::MAX).await?,
        )?;
        let projectless = ungrouped["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|thread| thread["codexThreadId"] == "00000000-0000-7000-8000-000000000003")
            .unwrap();
        assert!(projectless["project"].is_null());
        assert_eq!(
            projectless["cwdDisplay"],
            "/Users/demo/Documents/Codex/2026-08-28/generated-name"
        );
        Ok(())
    }

    fn cursor_test_error(error: CursorFailure) -> anyhow::Error {
        match error {
            CursorFailure::Invalid(message) => anyhow::anyhow!(message),
            CursorFailure::Internal(error) => error,
        }
    }
}
