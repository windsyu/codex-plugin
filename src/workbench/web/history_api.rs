use super::*;
use crate::workbench::recording::history::{HistoryError, Query as HistoryQuery};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Page {
    cursor: Option<String>,
    before: Option<u64>,
}

async fn query(state: Arc<WebState>, headers: HeaderMap, query: HistoryQuery) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    let Some(history) = state.hub.history() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "history_disabled");
    };
    let Ok(permit) = state.snapshot_slots.clone().try_acquire_owned() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "reading_busy");
    };
    let value = match history.query(query).await {
        Ok(value) => value,
        Err(reason) => {
            let (status, code) = match reason {
                HistoryError::Busy => (StatusCode::SERVICE_UNAVAILABLE, "history_busy"),
                HistoryError::Unavailable => {
                    (StatusCode::SERVICE_UNAVAILABLE, "history_unavailable")
                }
                HistoryError::NotFound => (StatusCode::NOT_FOUND, "history_not_found"),
                HistoryError::InvalidCursor => (StatusCode::BAD_REQUEST, "invalid_history_cursor"),
                HistoryError::StaleCursor => (StatusCode::CONFLICT, "history_changed"),
                HistoryError::Deleted => (StatusCode::GONE, "history_deleted"),
            };
            return error(status, code);
        }
    };
    let (parts, body) = Json(value).into_response().into_parts();
    let stream = async_stream::stream! {
        let _permit = permit;
        let mut body = body.into_data_stream();
        while let Some(bytes) = body.next().await { yield bytes; }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}
fn page(
    value: Result<Query<Page>, axum::extract::rejection::QueryRejection>,
) -> Result<Page, &'static str> {
    match value {
        Ok(Query(page)) if page.cursor.as_ref().is_none_or(|c| c.len() <= 200) => Ok(page),
        _ => Err("invalid_history_cursor"),
    }
}
pub(super) async fn status(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Path(epoch): Path<Uuid>,
) -> Response {
    query(state, headers, HistoryQuery::Status { epoch }).await
}
pub(super) async fn list(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    params: Result<Query<Page>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let params = match page(params) {
        Ok(p) => p,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    if params.before.is_some() {
        return error(StatusCode::BAD_REQUEST, "invalid_history_cursor");
    }
    query(
        state,
        headers,
        HistoryQuery::List {
            cursor: params.cursor,
        },
    )
    .await
}
pub(super) async fn read(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Path(epoch): Path<Uuid>,
    params: Result<Query<Page>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let params = match page(params) {
        Ok(p) => p,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    if params.cursor.is_some() {
        return error(StatusCode::BAD_REQUEST, "invalid_history_cursor");
    }
    query(
        state,
        headers,
        match params.before {
            Some(before) => HistoryQuery::Earlier { epoch, before },
            None => HistoryQuery::Run { epoch },
        },
    )
    .await
}
pub(super) async fn details(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Path((epoch, request)): Path<(Uuid, Uuid)>,
    params: Result<Query<Page>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let params = match page(params) {
        Ok(p) => p,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    query(
        state,
        headers,
        HistoryQuery::Details {
            epoch,
            request,
            cursor: params.cursor,
            before: params.before,
        },
    )
    .await
}
