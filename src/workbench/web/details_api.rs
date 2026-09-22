//! On-demand, paired, memory-only context reading. No capture or disk I/O here.
use super::*;
use crate::workbench::live::DetailError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DetailsQuery {
    epoch: Uuid,
    cursor: Option<String>,
}

pub(super) async fn read(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
    query: Result<Query<DetailsQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    let Ok(request_id) = Uuid::parse_str(&request_id) else {
        return error(StatusCode::BAD_REQUEST, "invalid_request_id");
    };
    let fail = |status, code| {
        (status, Json(serde_json::json!({"error":code,"runEpoch":state.hub.epoch(),"requestId":request_id}))).into_response()
    };
    let Ok(Query(query)) = query else {
        return fail(StatusCode::BAD_REQUEST, "invalid_details_query");
    };
    if query.epoch != state.hub.epoch() {
        return fail(StatusCode::CONFLICT, "run_epoch_changed");
    }
    if query
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.len() > 160)
    {
        return fail(StatusCode::BAD_REQUEST, "invalid_details_cursor");
    }
    let Ok(permit) = state.snapshot_slots.clone().try_acquire_owned() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "reading_busy");
    };
    let page = match state
        .hub
        .request_details(request_id, query.cursor.as_deref())
    {
        Ok(page) => page,
        Err(reason) => {
            let (status, code) = match reason {
                DetailError::NotFound => (StatusCode::NOT_FOUND, "request_not_observed"),
                DetailError::Evicted => (StatusCode::GONE, "request_details_evicted"),
                DetailError::InvalidCursor => (StatusCode::BAD_REQUEST, "invalid_details_cursor"),
                DetailError::StaleCursor => (StatusCode::CONFLICT, "request_details_changed"),
            };
            return fail(status, code);
        }
    };
    let (parts, body) = Json(page).into_response().into_parts();
    let stream = async_stream::stream! {
        let _permit = permit;
        let mut body = body.into_data_stream();
        while let Some(bytes) = body.next().await { yield bytes; }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}
