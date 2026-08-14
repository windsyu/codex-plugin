use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::StreamExt;
use rusqlite::types::Value as SqlValue;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::db::Database;
use crate::ingest::load_or_create_token;
use crate::model::{ApiEnvelope, Pagination};

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE_CSS: &str = include_str!("../web/style.css");

#[derive(Clone)]
struct ApiState {
    database: Arc<Database>,
    token: Arc<String>,
    strict_origin: bool,
    allowed_origins: Arc<Vec<String>>,
}

pub async fn serve(config: Config, database: Arc<Database>) -> Result<()> {
    let token = load_or_create_token(&config.server.bearer_token_file)?;
    let state = ApiState {
        database,
        token: Arc::new(token),
        strict_origin: config.server.strict_origin,
        allowed_origins: Arc::new(config.server.allowed_origins.clone()),
    };
    let protected = Router::new()
        .route("/health", get(health))
        .route("/sources", get(sources))
        .route("/threads", get(threads))
        .route("/threads/{thread_key}", get(thread_detail))
        .route("/threads/{thread_key}/turns", get(turns))
        .route("/threads/{thread_key}/items", get(items))
        .route("/threads/{thread_key}/events", get(thread_events))
        .route("/events", get(events))
        .route("/search", get(search))
        .route("/meta/capabilities", get(capabilities))
        .route("/stream", get(sse_stream))
        .route("/stream/ws", get(ws_stream))
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize));

    let app = Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .nest("/v1", protected)
        .with_state(state)
        .layer(CatchPanicLayer::new())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn(security_headers));

    let listener = TcpListener::bind(config.server.bind).await?;
    tracing::info!(address = %config.server.bind, token_file = %config.server.bearer_token_file.display(), "Observer Web Viewer ready");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn authorize(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token.as_bytes(), state.token.as_bytes()));
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

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn app_js() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
        .into_response()
}

async fn style_css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLE_CSS,
    )
        .into_response()
}

async fn health(State(state): State<ApiState>) -> Response {
    let seq = state.database.max_event_seq().unwrap_or(0);
    let sources = state.database.query_json(
        "SELECT source_id,kind,status,last_seen_at_ms,last_error_json FROM sources ORDER BY source_id", &[],
        |row| Ok(json!({
            "sourceId":row.get::<_,String>(0)?, "kind":row.get::<_,String>(1)?,
            "status":row.get::<_,String>(2)?, "lastSeenAtMs":row.get::<_,Option<i64>>(3)?,
            "lastError":row.get::<_,Option<String>>(4)?
        })),
    ).unwrap_or_default();
    Json(ApiEnvelope::new(seq, json!({
        "status": if sources.iter().any(|source| source["status"] == "degraded") { "degraded" } else { "healthy" },
        "ready":true,"database":{"migration":"ok","wal":"ok"},"ingest":{"projectionLag":0},
        "sources":sources,"liveMode":"off"
    }))).into_response()
}

async fn sources(State(state): State<ApiState>) -> Response {
    envelope_query(
        &state,
        "SELECT source_id,kind,stable_identity,status,last_seen_at_ms FROM sources ORDER BY source_id",
        &[],
        |row| {
            Ok(
                json!({"sourceId":row.get::<_,String>(0)?,"kind":row.get::<_,String>(1)?,
            "stableIdentity":row.get::<_,String>(2)?,"status":row.get::<_,String>(3)?,"lastSeenAtMs":row.get::<_,Option<i64>>(4)?}),
            )
        },
    )
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
          recency_at_ms,last_message_preview,last_event_seq FROM threads WHERE thread_key=?1",
        &[&thread_key], thread_row,
    );
    match rows {
        Ok(mut rows) if !rows.is_empty() => {
            let seq = state.database.max_event_seq().unwrap_or(0);
            Json(ApiEnvelope::new(
                seq,
                json!({
                    "thread":rows.remove(0),"sources":[],"coverageSummary":{},
                    "relations":{"parent":null,"forkedFrom":null,"children":[]},
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
    Query(page): Query<Pagination>,
) -> Response {
    let limit = page.limit.unwrap_or(100).clamp(1, 200) as i64;
    envelope_query(&state,
        "SELECT turn_id,status,capture_completeness,completeness_reasons_json,coverage_json,started_at_ms,
          completed_at_ms,execution_context_json,projection_json,last_event_seq FROM turns WHERE thread_key=?1
          ORDER BY started_at_ms,turn_id LIMIT ?2",
        &[&thread_key,&limit], |row| Ok(json!({
            "turnId":row.get::<_,String>(0)?,"status":row.get::<_,String>(1)?,
            "captureCompleteness":row.get::<_,String>(2)?,"completenessReasons":parse_json(row.get::<_,String>(3)?),
            "coverage":parse_json(row.get::<_,String>(4)?),"startedAtMs":row.get::<_,Option<i64>>(5)?,
            "completedAtMs":row.get::<_,Option<i64>>(6)?,"executionContext":parse_optional_json(row.get::<_,Option<String>>(7)?),
            "raw":parse_json(row.get::<_,String>(8)?),"lastEventSeq":row.get::<_,i64>(9)?
        })))
}

async fn items(
    State(state): State<ApiState>,
    Path(thread_key): Path<String>,
    Query(page): Query<Pagination>,
) -> Response {
    let limit = page.limit.unwrap_or(200).clamp(1, 200) as i64;
    envelope_query(&state,
        "SELECT turn_scope,item_id,turn_id,item_type,status,started_at_ms,completed_at_ms,summary_text,
          projection_json,provenance_json,last_event_seq FROM items WHERE thread_key=?1 ORDER BY started_at_ms,item_id LIMIT ?2",
        &[&thread_key,&limit], |row| Ok(json!({
            "turnScope":row.get::<_,String>(0)?,"itemId":row.get::<_,String>(1)?,"turnId":row.get::<_,Option<String>>(2)?,
            "itemType":row.get::<_,String>(3)?,"status":row.get::<_,String>(4)?,"startedAtMs":row.get::<_,Option<i64>>(5)?,
            "completedAtMs":row.get::<_,Option<i64>>(6)?,"summaryText":row.get::<_,Option<String>>(7)?,
            "raw":parse_json(row.get::<_,String>(8)?),"provenance":parse_json(row.get::<_,String>(9)?),"lastEventSeq":row.get::<_,i64>(10)?
        })))
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
          turn_id,item_id,method,phase,durability,raw_json,redaction_json,decode_status,decode_error,stored_raw_hash
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
    runtime_status: Option<String>,
    capture_completeness: Option<String>,
    archived: Option<bool>,
    q: Option<String>,
    sort: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadCursor {
    endpoint: String,
    query_fingerprint: String,
    as_of_event_seq: i64,
    last_recency_at_ms: i64,
    last_thread_key: String,
}

enum CursorFailure {
    Invalid(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for CursorFailure {
    fn from(value: anyhow::Error) -> Self {
        Self::Internal(value)
    }
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
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
    let expression = search_expression(&query.q);
    let limit = query.limit.unwrap_or(50).clamp(1, 200) as i64;
    envelope_query(&state,
        "SELECT entity_key,thread_key,item_id,snippet(search_index,3,'','', ' … ',20),bm25(search_index)
         FROM search_index WHERE search_index MATCH ?1 ORDER BY rank LIMIT ?2",
        &[&expression,&limit], |row| Ok(json!({
            "entityKey":row.get::<_,String>(0)?,"threadKey":row.get::<_,String>(1)?,
            "itemId":row.get::<_,String>(2)?,"snippet":row.get::<_,String>(3)?,"score":row.get::<_,f64>(4)?
        })))
}

async fn capabilities(State(state): State<ApiState>) -> Response {
    let seq = state.database.max_event_seq().unwrap_or(0);
    Json(ApiEnvelope::new(
        seq,
        json!({
            "observerVersion":env!("CARGO_PKG_VERSION"),"apiVersion":"v1","readOnly":true,
            "store":{"plainJsonl":true,"zstdJsonl":true,"incrementalRescan":true},
            "live":{"enabled":false,"reason":"live_mode_off"},
            "streams":{"sse":true,"webSocket":true},"mutationRoutes":[]
        }),
    ))
    .into_response()
}

async fn sse_stream(State(state): State<ApiState>, Query(query): Query<EventQuery>) -> Response {
    let mut cursor = query.after_event_seq.unwrap_or(0);
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
    let stream = async_stream::stream! {
        loop {
            let rows = database.query_json(
                "SELECT event_seq,event_id,source_id,epoch_id,source_seq,observed_at_ms,event_at_ms,thread_key,codex_thread_id,
                  turn_id,item_id,method,phase,durability,raw_json,redaction_json,decode_status,decode_error,stored_raw_hash
                  FROM raw_events WHERE event_seq>?1 ORDER BY event_seq LIMIT 200",
                &[&cursor], event_row,
            ).unwrap_or_default();
            if rows.is_empty() {
                tokio::time::sleep(Duration::from_millis(750)).await;
                continue;
            }
            for row in rows {
                cursor = row["eventSeq"].as_i64().unwrap_or(cursor);
                yield Ok::<Event, Infallible>(Event::default().id(cursor.to_string()).event("event").json_data(row).unwrap());
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
    ws.on_upgrade(move |socket| handle_socket(socket, state.database))
        .into_response()
}

async fn handle_socket(mut socket: WebSocket, database: Arc<Database>) {
    let Some(Ok(Message::Text(text))) = socket.next().await else {
        return;
    };
    let request: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if request.get("type").and_then(Value::as_str) != Some("subscribe") {
        let _ = socket
            .send(Message::Text(
                json!({"type":"error","code":"CURSOR_INVALID"})
                    .to_string()
                    .into(),
            ))
            .await;
        return;
    }
    let mut cursor = request
        .get("afterEventSeq")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if database
        .retention_low_watermark()
        .is_ok_and(|low_watermark| cursor < low_watermark)
    {
        let _ = socket
            .send(Message::Text(
                json!({"type":"error","code":"CURSOR_EXPIRED"})
                    .to_string()
                    .into(),
            ))
            .await;
        return;
    }
    if socket
        .send(Message::Text(
            json!({"type":"subscribed","afterEventSeq":cursor})
                .to_string()
                .into(),
        ))
        .await
        .is_err()
    {
        return;
    }
    loop {
        let rows = database.query_json(
            "SELECT event_seq,event_id,source_id,epoch_id,source_seq,observed_at_ms,event_at_ms,thread_key,codex_thread_id,
              turn_id,item_id,method,phase,durability,raw_json,redaction_json,decode_status,decode_error,stored_raw_hash
              FROM raw_events WHERE event_seq>?1 ORDER BY event_seq LIMIT 200", &[&cursor], event_row,
        ).unwrap_or_default();
        for row in rows {
            cursor = row["eventSeq"].as_i64().unwrap_or(cursor);
            let frame = json!({"type":"event","eventSeq":cursor,"data":row});
            if socket
                .send(Message::Text(frame.to_string().into()))
                .await
                .is_err()
            {
                return;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(750)) => {},
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    _ => {}
                }
            }
        }
    }
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
          recency_at_ms,last_message_preview,last_event_seq FROM threads WHERE last_event_seq <= ?",
    );
    let mut parameters = vec![SqlValue::Integer(as_of)];
    if let Some(source_id) = query.source_id {
        sql.push_str(" AND store_source_id = ?");
        parameters.push(SqlValue::Text(source_id));
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

fn thread_query_fingerprint(query: &ThreadQuery) -> String {
    let canonical = json!({
        "sourceId":query.source_id,
        "runtimeStatus":query.runtime_status,
        "captureCompleteness":query.capture_completeness,
        "archived":query.archived,
        "q":query.q,
        "sort":query.sort.as_deref().unwrap_or("recency_desc")
    });
    blake3::hash(canonical.to_string().as_bytes())
        .to_hex()
        .to_string()
}

fn cursor_key(token: &str) -> Result<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD.decode(token)?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("bearer token must decode to 32 bytes"))
}

fn encode_cursor(cursor: &ThreadCursor, token: &str) -> Result<String> {
    let payload = serde_json::to_vec(cursor)?;
    let signature = blake3::keyed_hash(&cursor_key(token)?, &payload);
    Ok(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(payload),
        URL_SAFE_NO_PAD.encode(signature.as_bytes())
    ))
}

fn decode_cursor(value: &str, token: &str) -> Result<ThreadCursor> {
    let (payload, signature) = value
        .split_once('.')
        .ok_or_else(|| anyhow::anyhow!("cursor has an invalid envelope"))?;
    let payload = URL_SAFE_NO_PAD.decode(payload)?;
    let signature = URL_SAFE_NO_PAD.decode(signature)?;
    let expected = blake3::keyed_hash(&cursor_key(token)?, &payload);
    if !constant_time_eq(&signature, expected.as_bytes()) {
        anyhow::bail!("cursor signature is invalid");
    }
    Ok(serde_json::from_slice(&payload)?)
}

fn search_expression(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn thread_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "threadKey":row.get::<_,String>(0)?,"codexThreadId":row.get::<_,String>(1)?,"storeSourceId":row.get::<_,String>(2)?,
        "name":row.get::<_,Option<String>>(3)?,"cwdDisplay":row.get::<_,Option<String>>(4)?,"source":row.get::<_,Option<String>>(5)?,
        "model":row.get::<_,Option<String>>(6)?,"archived":row.get::<_,bool>(7)?,"status":row.get::<_,Option<String>>(8)?,
        "stale":row.get::<_,bool>(9)?,"captureCompleteness":row.get::<_,String>(10)?,
        "completenessReasons":parse_json(row.get::<_,String>(11)?),"createdAtMs":row.get::<_,Option<i64>>(12)?,
        "updatedAtMs":row.get::<_,Option<i64>>(13)?,"recencyAtMs":row.get::<_,Option<i64>>(14)?,
        "lastMessagePreview":row.get::<_,Option<String>>(15)?,"lastEventSeq":row.get::<_,i64>(16)?
    }))
}

fn event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "eventSeq":row.get::<_,i64>(0)?,"eventId":row.get::<_,String>(1)?,"sourceId":row.get::<_,String>(2)?,
        "sourceEpoch":row.get::<_,String>(3)?,"sourceSeq":row.get::<_,i64>(4)?,"observedAtMs":row.get::<_,i64>(5)?,
        "eventAtMs":row.get::<_,Option<i64>>(6)?,"threadKey":row.get::<_,String>(7)?,"codexThreadId":row.get::<_,String>(8)?,
        "turnId":row.get::<_,Option<String>>(9)?,"itemId":row.get::<_,Option<String>>(10)?,"method":row.get::<_,String>(11)?,
        "phase":row.get::<_,String>(12)?,"durability":row.get::<_,String>(13)?,"raw":parse_json(row.get::<_,String>(14)?),
        "redaction":parse_json(row.get::<_,String>(15)?),"decodeStatus":row.get::<_,String>(16)?,
        "decodeError":row.get::<_,Option<String>>(17)?,"storedRawHash":row.get::<_,String>(18)?
    }))
}

fn diagnostics(database: &Database, thread_key: &str) -> Value {
    let rows = database.query_json(
        "SELECT SUM(CASE WHEN decode_status='error' THEN 1 ELSE 0 END),
          SUM(CASE WHEN decode_status='unknown' THEN 1 ELSE 0 END) FROM raw_events WHERE thread_key=?1",
        &[&thread_key], |row| Ok(json!({"decodeErrors":row.get::<_,Option<i64>>(0)?.unwrap_or(0),
            "unknownVariants":row.get::<_,Option<i64>>(1)?.unwrap_or(0),"conflicts":0})),
    ).unwrap_or_default();
    rows.into_iter()
        .next()
        .unwrap_or_else(|| json!({"decodeErrors":0,"unknownVariants":0,"conflicts":0}))
}

fn parse_json(value: String) -> Value {
    serde_json::from_str(&value).unwrap_or(Value::Null)
}
fn parse_optional_json(value: Option<String>) -> Value {
    value.map(parse_json).unwrap_or(Value::Null)
}

fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"apiVersion":"v1","error":{"code":code,"message":message,"details":{}}})),
    )
        .into_response()
}

fn internal_error(error: anyhow::Error) -> Response {
    tracing::error!(error = %error, "API query failed");
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "INTERNAL_ERROR",
        "internal query error",
    )
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn token_comparison_checks_length_and_content() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"other"));
        assert!(!constant_time_eq(b"token", b"token-long"));
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
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([9_u8; 32])),
            strict_origin: true,
            allowed_origins: Arc::new(Vec::new()),
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
            database,
            token: Arc::new(URL_SAFE_NO_PAD.encode([9_u8; 32])),
            strict_origin: true,
            allowed_origins: Arc::new(Vec::new()),
        };
        let response = event_query(&state, 4, Some(10), None, None, None);
        assert_eq!(response.status(), StatusCode::GONE);
        Ok(())
    }

    fn cursor_test_error(error: CursorFailure) -> anyhow::Error {
        match error {
            CursorFailure::Invalid(message) => anyhow::anyhow!(message),
            CursorFailure::Internal(error) => error,
        }
    }
}
