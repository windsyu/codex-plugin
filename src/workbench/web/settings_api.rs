use super::*;
use crate::workbench::config::{Config, ConfigError, Settings};

fn failure(epoch: Uuid, status: StatusCode, error: ConfigError) -> Response {
    (status,Json(serde_json::json!({"error":{"source":"workbench_settings","currentRunEpoch":epoch,"code":error.code,"field":error.field,"line":error.line,"column":error.column}}))).into_response()
}
fn result(state: &WebState, value: Result<Settings, ConfigError>) -> Response {
    match value {
        Ok(settings) => {
            let revision = settings.revision.clone();
            let mut response =
                Json(serde_json::json!({"currentRunEpoch":state.hub.epoch(),"settings":settings}))
                    .into_response();
            if let Some(r) = revision {
                response.headers_mut().insert(
                    header::ETAG,
                    HeaderValue::from_str(&format!("\"{r}\"")).expect("hash ETag"),
                );
            }
            response
        }
        Err(e) => {
            let status = match e.code {
                "config_changed" => StatusCode::PRECONDITION_FAILED,
                "config_io_error"
                | "config_busy"
                | "config_unavailable"
                | "config_save_unconfirmed" => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::UNPROCESSABLE_ENTITY,
            };
            failure(state.hub.epoch(), status, e)
        }
    }
}
pub(super) async fn read(State(state): State<Arc<WebState>>, headers: HeaderMap) -> Response {
    if !owner_authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    let Some(config) = &state.options.settings else {
        return error(StatusCode::NOT_FOUND, "settings_unavailable");
    };
    result(&state, config.read().await)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Save {
    current_run_epoch: Uuid,
    config: Box<serde_json::value::RawValue>,
}

pub(super) async fn save(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !owner_authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    if !same_origin(&headers, &state) {
        return error(StatusCode::FORBIDDEN, "origin_required");
    }
    if headers.get_all(header::CONTENT_TYPE).iter().count() != 1
        || headers
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.split(';').next())
            .map(str::trim)
            != Some("application/json")
    {
        return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required");
    }
    let Some(config) = &state.options.settings else {
        return error(StatusCode::NOT_FOUND, "settings_unavailable");
    };
    let Ok(request) = serde_json::from_slice::<Save>(&body) else {
        return failure(
            state.hub.epoch(),
            StatusCode::BAD_REQUEST,
            ConfigError::field("$", "invalid_config_request"),
        );
    };
    if request.current_run_epoch != state.hub.epoch() {
        return error(StatusCode::CONFLICT, "stale_run");
    }
    let Some(raw_revision) = headers.get(header::IF_MATCH).and_then(|h| h.to_str().ok()) else {
        return error(
            StatusCode::PRECONDITION_REQUIRED,
            "config_revision_required",
        );
    };
    if headers.get_all(header::IF_MATCH).iter().count() != 1 {
        return error(StatusCode::BAD_REQUEST, "invalid_config_revision");
    }
    let Some(revision) = raw_revision
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return error(StatusCode::BAD_REQUEST, "invalid_config_revision");
    };
    let value = match Config::parse(request.config.get().as_bytes()) {
        Ok(value) => value,
        Err(e) => return failure(state.hub.epoch(), StatusCode::UNPROCESSABLE_ENTITY, e),
    };
    result(&state, config.save(revision.into(), value).await)
}
