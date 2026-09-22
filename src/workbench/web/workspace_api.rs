use super::*;
use crate::workbench::workspace::{Fault, Query as WorkspaceQuery};

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Params {
    path: Option<String>,
    cursor: Option<String>,
    q: Option<String>,
    regex: Option<bool>,
    case_sensitive: Option<bool>,
    scope: Option<String>,
}
pub(super) async fn read(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    params: std::result::Result<Query<Params>, axum::extract::rejection::QueryRejection>,
) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    let Some(workspace) = &state.options.workspace else {
        return error(StatusCode::NOT_FOUND, "workspace_unavailable");
    };
    let Ok(Query(p)) = params else {
        return error(StatusCode::BAD_REQUEST, "invalid_workspace_query");
    };
    let query = match uri
        .path()
        .strip_prefix("/workbench/v1/workspace/")
        .unwrap_or("")
    {
        "files"
            if p.q.is_none()
                && p.scope.is_none()
                && p.regex.is_none()
                && p.case_sensitive.is_none() =>
        {
            WorkspaceQuery::Files {
                path: p.path.unwrap_or_default(),
                cursor: p.cursor,
            }
        }
        "file"
            if p.path.is_some()
                && p.cursor.is_none()
                && p.q.is_none()
                && p.scope.is_none()
                && p.regex.is_none()
                && p.case_sensitive.is_none() =>
        {
            WorkspaceQuery::File {
                path: p.path.unwrap(),
            }
        }
        "search"
            if p.q.is_some() && p.path.is_none() && p.cursor.is_none() && p.scope.is_none() =>
        {
            WorkspaceQuery::Search {
                text: p.q.unwrap(),
                regex: p.regex.unwrap_or(false),
                case_sensitive: p.case_sensitive.unwrap_or(false),
            }
        }
        "git/status"
            if p.path.is_none()
                && p.cursor.is_none()
                && p.q.is_none()
                && p.scope.is_none()
                && p.regex.is_none()
                && p.case_sensitive.is_none() =>
        {
            WorkspaceQuery::GitStatus
        }
        "git/log"
            if p.path.is_none()
                && p.q.is_none()
                && p.scope.is_none()
                && p.regex.is_none()
                && p.case_sensitive.is_none() =>
        {
            WorkspaceQuery::GitLog { cursor: p.cursor }
        }
        "git/diff"
            if p.path.is_some()
                && p.cursor.is_none()
                && p.q.is_none()
                && p.regex.is_none()
                && p.case_sensitive.is_none()
                && matches!(p.scope.as_deref(), Some("working" | "staged")) =>
        {
            WorkspaceQuery::GitDiff {
                path: p.path.unwrap(),
                staged: p.scope.as_deref() == Some("staged"),
            }
        }
        _ => return error(StatusCode::BAD_REQUEST, "invalid_workspace_query"),
    };
    match workspace.query(query).await {
        Ok(mut value) => {
            value["currentRunEpoch"] = serde_json::json!(state.hub.epoch());
            Json(value).into_response()
        }
        Err(Fault(code)) => {
            let status = match code {
                "forbidden_path" => StatusCode::FORBIDDEN,
                "not_found" | "not_git_repository" => StatusCode::NOT_FOUND,
                "invalid_cursor" | "invalid_search" | "invalid_regex" => StatusCode::BAD_REQUEST,
                "workspace_changed" | "file_changed" | "git_conflict" => StatusCode::CONFLICT,
                "file_too_large"
                | "directory_too_large"
                | "git_output_limit"
                | "program_output_limit" => StatusCode::PAYLOAD_TOO_LARGE,
                "binary_file" | "unsupported_path" => StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "workspace_timeout" => StatusCode::GATEWAY_TIMEOUT,
                _ => StatusCode::SERVICE_UNAVAILABLE,
            };
            (status, Json(serde_json::json!({"error":{"source":"workbench_workspace","currentRunEpoch":state.hub.epoch(),"code":code}}))).into_response()
        }
    }
}
