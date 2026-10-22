use super::*;
use crate::workbench::application::{directories::BrowseRequest, management};

pub(super) async fn control(
    State(state): State<Arc<StateData>>,
    auth: Auth,
    body: Bytes,
) -> Response {
    if auth.remote.is_some() {
        return error(StatusCode::FORBIDDEN, "local_management_required");
    }
    if !origin(&auth, &state) {
        return error(StatusCode::FORBIDDEN, "origin_required");
    }
    if body.len() > 1024
        || auth.get_all(header::CONTENT_TYPE).iter().count() != 1
        || auth.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) != Some("application/json")
    {
        return error(StatusCode::BAD_REQUEST, "invalid_control_request");
    }
    let Ok(request) = serde_json::from_slice::<management::Request>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_control_request");
    };
    if !matches_secret(&request.token, &state.owner.pairing) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    if request.instance_id != state.id {
        return error(StatusCode::CONFLICT, "stale_instance");
    }
    if matches!(request.action, management::Action::Shutdown) {
        state.shutdown_requested.store(true, Ordering::Release);
    }
    let relay = state.relay.read().unwrap().clone();
    let relay = if let Some(relay) = relay {
        let snapshot = relay.status().await;
        Some(management::RelayStatus {
            registered: snapshot.gateway_id.is_some(),
            enabled: snapshot.enabled,
            authentication_required: snapshot.state == "authenticationRequired",
            state: snapshot.state,
        })
    } else {
        None
    };
    Json(management::Status {
        instance_id: state.id,
        process_id: std::process::id(),
        service_label: state.service_label.read().unwrap().clone(),
        stopping: state.shutdown_requested.load(Ordering::Acquire),
        active_runs: state
            .run
            .read()
            .unwrap()
            .iter()
            .filter(|(r, _, _)| r.state == "running")
            .count(),
        relay,
    })
    .into_response()
}

pub(super) async fn directories(
    State(state): State<Arc<StateData>>,
    auth: Auth,
    body: Bytes,
) -> Response {
    if let Err((status, code)) = launch_owner(&auth, &state) {
        return error(status, code);
    }
    if body.len() > 8192 {
        return error(StatusCode::BAD_REQUEST, "invalid_directory_request");
    }
    let Ok(request) = serde_json::from_slice::<BrowseRequest>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_directory_request");
    };
    match state.directories.list(request, state.id).await {
        Ok(value) => {
            if !authorised(&auth, &state) {
                return error(StatusCode::UNAUTHORIZED, "access_revoked");
            }
            Json(value).into_response()
        }
        Err(code) => error(
            match code {
                "stale_instance" | "directory_changed" | "directory_cursor_expired" => {
                    StatusCode::CONFLICT
                }
                "directory_busy" => StatusCode::SERVICE_UNAVAILABLE,
                // Relay 401/403 means revoked authentication; OS directory
                // permission failures remain browse errors, never account logout.
                _ => StatusCode::UNPROCESSABLE_ENTITY,
            },
            code,
        ),
    }
}
