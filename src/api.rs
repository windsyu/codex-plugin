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
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
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

async fn threads(State(state): State<ApiState>, Query(page): Query<Pagination>) -> Response {
    let limit = page.limit.unwrap_or(50).clamp(1, 200) as i64;
    envelope_query(&state,
        "SELECT thread_key,codex_thread_id,store_source_id,name,cwd,source,model,archived,runtime_status,
          runtime_status_stale,capture_completeness,completeness_reasons_json,created_at_ms,updated_at_ms,
          recency_at_ms,last_message_preview,last_event_seq FROM threads ORDER BY recency_at_ms DESC,thread_key LIMIT ?1",
        &[&limit], thread_row)
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
    let expression = format!("\"{}\"", query.q.replace('"', "\"\""));
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

async fn sse_stream(
    State(state): State<ApiState>,
    Query(query): Query<EventQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut cursor = query.after_event_seq.unwrap_or(0);
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
                yield Ok(Event::default().id(cursor.to_string()).event("event").json_data(row).unwrap());
            }
        }
    };
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    )
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

    #[test]
    fn token_comparison_checks_length_and_content() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"other"));
        assert!(!constant_time_eq(b"token", b"token-long"));
    }
}
