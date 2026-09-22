use super::*;
use crate::workbench::recording::management::{
    Error as ManagementError, PreviewMode, Query as ManagementQuery,
};

async fn execute(
    state: &WebState,
    headers: &HeaderMap,
    query: ManagementQuery,
    accepted: bool,
) -> Response {
    if !authorised(headers, state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    let Some(handle) = &state.options.management else {
        return error(StatusCode::NOT_FOUND, "history_management_unavailable");
    };
    let (stage, job, preview) = match &query {
        ManagementQuery::Usage { .. } => ("usage", None, None),
        ManagementQuery::Refresh => ("refresh", None, None),
        ManagementQuery::Preview { .. } => ("preview", None, None),
        ManagementQuery::ReadPreview { id } => ("read_preview", None, Some(*id)),
        ManagementQuery::CreateJob {
            preview, operation, ..
        } => ("create_job", Some(*operation), Some(*preview)),
        ManagementQuery::Job { id } => ("read_job", Some(*id), None),
        ManagementQuery::Jobs { .. } => ("list_jobs", None, None),
        ManagementQuery::Cancel { id } => ("cancel_job", Some(*id), None),
        #[cfg(test)]
        ManagementQuery::HoldWorker { .. } => ("test", None, None),
    };
    match handle.query(query).await {
        Ok(value) => (
            if accepted {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            },
            Json(value),
        )
            .into_response(),
        Err(reason) => {
            let (status, code) = match reason {
                ManagementError::Busy => {
                    (StatusCode::SERVICE_UNAVAILABLE, "history_management_busy")
                }
                ManagementError::Unavailable => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "history_management_unavailable",
                ),
                ManagementError::NotFound => {
                    (StatusCode::NOT_FOUND, "history_management_not_found")
                }
                ManagementError::Invalid => (
                    StatusCode::BAD_REQUEST,
                    "invalid_history_management_request",
                ),
                ManagementError::Stale => (StatusCode::CONFLICT, "history_management_changed"),
                ManagementError::Disabled => (StatusCode::FORBIDDEN, "cleanup_disabled"),
            };
            (status,Json(serde_json::json!({"error":{"source":"workbench_history_management","currentRunEpoch":state.hub.epoch(),"code":code,"stage":stage,"jobId":job,"previewId":preview}}))).into_response()
        }
    }
}
fn write_error(state: &WebState, headers: &HeaderMap) -> Option<Response> {
    if !authorised(headers, state) {
        return Some(error(StatusCode::UNAUTHORIZED, "pairing_required"));
    }
    if !same_origin(headers, state) {
        return Some(error(StatusCode::FORBIDDEN, "origin_required"));
    }
    if headers.get_all(header::CONTENT_TYPE).iter().count() != 1
        || headers
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.split(';').next())
            .map(str::trim)
            != Some("application/json")
    {
        return Some(error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required"));
    }
    None
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Page {
    cursor: Option<String>,
}
pub(super) async fn usage(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    page: Result<Query<Page>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let cursor = match page {
        Ok(Query(page)) if page.cursor.as_ref().is_none_or(|s| s.len() <= 128) => page.cursor,
        _ => return error(StatusCode::BAD_REQUEST, "invalid_usage_cursor"),
    };
    execute(&state, &headers, ManagementQuery::Usage { cursor }, false).await
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Current {
    current_run_epoch: Uuid,
}
pub(super) async fn refresh(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(error) = write_error(&state, &headers) {
        return error;
    }
    let Ok(request) = serde_json::from_slice::<Current>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_usage_request");
    };
    if request.current_run_epoch != state.hub.epoch() {
        return error(StatusCode::CONFLICT, "stale_run");
    }
    execute(&state, &headers, ManagementQuery::Refresh, true).await
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewRequest {
    current_run_epoch: Uuid,
    run_epochs: Option<Vec<Uuid>>,
    mode: Option<String>,
    days: Option<u32>,
}
pub(super) async fn preview(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(error) = write_error(&state, &headers) {
        return error;
    }
    let Ok(request) = serde_json::from_slice::<PreviewRequest>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_cleanup_preview");
    };
    if request.current_run_epoch != state.hub.epoch() {
        return error(StatusCode::CONFLICT, "stale_run");
    }
    if request
        .run_epochs
        .as_ref()
        .is_some_and(|ids| ids.len() > 100)
    {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "cleanup_batch_too_large");
    }
    let mode = match (request.run_epochs, request.mode.as_deref(), request.days) {
        (Some(ids), None, None) => PreviewMode::Manual(ids),
        (None, Some("retention"), Some(days)) => PreviewMode::Retention(days),
        _ => return error(StatusCode::BAD_REQUEST, "invalid_cleanup_preview"),
    };
    execute(&state, &headers, ManagementQuery::Preview { mode }, true).await
}
pub(super) async fn read_preview(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    execute(&state, &headers, ManagementQuery::ReadPreview { id }, false).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateJob {
    current_run_epoch: Uuid,
    preview_id: Uuid,
    config_revision: String,
    operation_id: Uuid,
}
pub(super) async fn create_job(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(error) = write_error(&state, &headers) {
        return error;
    }
    let Ok(r) = serde_json::from_slice::<CreateJob>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_cleanup_job");
    };
    if r.current_run_epoch != state.hub.epoch() {
        return error(StatusCode::CONFLICT, "stale_run");
    }
    if r.config_revision.len() != 64 || !r.config_revision.bytes().all(|c| c.is_ascii_hexdigit()) {
        return error(StatusCode::BAD_REQUEST, "invalid_config_revision");
    }
    execute(
        &state,
        &headers,
        ManagementQuery::CreateJob {
            preview: r.preview_id,
            revision: r.config_revision,
            operation: r.operation_id,
        },
        true,
    )
    .await
}
pub(super) async fn read_job(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    execute(&state, &headers, ManagementQuery::Job { id }, false).await
}
pub(super) async fn jobs(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    page: Result<Query<Page>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let cursor = match page {
        Ok(Query(p)) if p.cursor.as_ref().is_none_or(|c| c.len() <= 128) => p.cursor,
        _ => return error(StatusCode::BAD_REQUEST, "invalid_job_cursor"),
    };
    execute(&state, &headers, ManagementQuery::Jobs { cursor }, false).await
}
pub(super) async fn cancel(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Bytes,
) -> Response {
    if let Some(error) = write_error(&state, &headers) {
        return error;
    }
    let Ok(r) = serde_json::from_slice::<Current>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_cleanup_job");
    };
    if r.current_run_epoch != state.hub.epoch() {
        return error(StatusCode::CONFLICT, "stale_run");
    }
    execute(&state, &headers, ManagementQuery::Cancel { id }, true).await
}
