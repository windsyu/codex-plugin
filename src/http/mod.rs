use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
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
use crate::credentials::rotate_token;
use crate::domain::model::ApiEnvelope;
use crate::domain::project::{project_key, project_name};
use crate::store::{Database, LATEST_SCHEMA_VERSION};
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
    strict_origin: bool,
    allowed_origins: Arc<Vec<String>>,
    live_modes: Arc<Vec<String>>,
    blob_downloads: Arc<Semaphore>,
    writer: WriterHandle,
    settings: Arc<Value>,
    tailscale: Option<Arc<ServeAccess>>,
}

pub async fn serve(
    config: Config,
    database: Arc<Database>,
    writer: WriterHandle,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
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
    let viewer_url = generate_pairing_url(bound_address, &token)?;
    let state = ApiState {
        database,
        token: Arc::new(token),
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

    let app = Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(web_asset))
        .route("/v1/auth/pair", post(pair_auth))
        .nest("/v1", protected)
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

async fn authorize(State(state): State<ApiState>, request: Request, next: Next) -> Response {
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
    let tailscale_authorized = state
        .tailscale
        .as_ref()
        .is_some_and(|access| tailscale_request_is_authorized(access, &request));
    let authorized = bearer_authorized || cookie_authorized || tailscale_authorized;
    if !authorized {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "valid bearer token required",
        );
    }
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
        return api_error(
            StatusCode::FORBIDDEN,
            "ORIGIN_REJECTED",
            "request origin is not allowed",
        );
    }
    next.run(request).await
}

fn tailscale_request_is_authorized(access: &ServeAccess, request: &Request) -> bool {
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
    let identified_user = headers
        .get("tailscale-user-login")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|login| !login.trim().is_empty());
    loopback_peer && matching_host && forwarded_https && identified_user
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
             GROUP BY project_key
             ORDER BY MAX(COALESCE(recency_at_ms,0)) DESC,project_key",
        &[],
        |row| {
            let key = match row.get::<_, Option<String>>(0)? {
                Some(key) if !key.is_empty() => key,
                _ => row
                    .get::<_, Option<String>>(1)?
                    .as_deref()
                    .map(project_key)
                    .unwrap_or_else(|| "unknown".to_string()),
            };
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
                "SELECT source_id,epoch_id,request_id,request_type,state,request_event_seq,resolved_event_seq
                 FROM pending_requests WHERE thread_key=?1 ORDER BY request_event_seq DESC LIMIT 100",
                &[&thread_key], |row| Ok(json!({"sourceId":row.get::<_,String>(0)?,"epochId":row.get::<_,String>(1)?,
                    "requestId":row.get::<_,String>(2)?,"requestType":row.get::<_,String>(3)?,"state":row.get::<_,String>(4)?,
                    "requestEventSeq":row.get::<_,i64>(5)?,"resolvedEventSeq":row.get::<_,Option<i64>>(6)?})),
            ).unwrap_or_default();
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
    let key = project_key_value
        .filter(|value| !value.is_empty())
        .or_else(|| cwd.as_deref().map(project_key))
        .unwrap_or_else(|| "unknown".to_string());
    let name = project_name(&key);
    let path = cwd.clone().unwrap_or_else(|| key.clone());

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
        "project":{"key":key,"name":name,"path":path},
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
    use tempfile::TempDir;

    #[test]
    fn token_comparison_checks_length_and_content() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"other"));
        assert!(!constant_time_eq(b"token", b"token-long"));
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

        assert!(tailscale_request_is_authorized(
            &access,
            &request(
                "127.0.0.1:50000",
                "observer.example.ts.net",
                Some("user@example.com")
            )?
        ));
        assert!(!tailscale_request_is_authorized(
            &access,
            &request(
                "100.64.0.1:50000",
                "observer.example.ts.net",
                Some("user@example.com")
            )?
        ));
        assert!(!tailscale_request_is_authorized(
            &access,
            &request(
                "127.0.0.1:50000",
                "wrong.example.ts.net",
                Some("user@example.com")
            )?
        ));
        assert!(!tailscale_request_is_authorized(
            &access,
            &request("127.0.0.1:50000", "observer.example.ts.net", None)?
        ));
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
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([9_u8; 32])),
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
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([6_u8; 32])),
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
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([8_u8; 32])),
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
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([9_u8; 32])),
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
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([5_u8; 32])),
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
            database,
            token: Arc::new("token".into()),
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
            database,
            token: Arc::new("token".into()),
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
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([9_u8; 32])),
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
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([9_u8; 32])),
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
        let projects = body["data"].as_array().unwrap();
        assert_eq!(projects.len(), 2);
        let local = projects
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
        Ok(())
    }

    fn cursor_test_error(error: CursorFailure) -> anyhow::Error {
        match error {
            CursorFailure::Invalid(message) => anyhow::anyhow!(message),
            CursorFailure::Internal(error) => error,
        }
    }
}
