//! Explicit local management; public ingress cannot bind or recover credentials.
use super::*;

fn service(
    state: &StateData,
    auth: &Auth,
) -> std::result::Result<crate::workbench::relay::Service, (StatusCode, &'static str)> {
    if auth.remote.is_some() || !authorised(auth, state) {
        return Err((StatusCode::FORBIDDEN, "local_management_required"));
    }
    state
        .relay
        .read()
        .unwrap()
        .clone()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "relay_unavailable"))
}
fn response(state: &StateData, relay: crate::workbench::relay::Snapshot) -> Response {
    Json(serde_json::json!({"instanceId":state.id,"relay":relay})).into_response()
}
pub(super) async fn status(State(state): State<Arc<StateData>>, auth: Auth) -> Response {
    match service(&state, &auth) {
        Ok(service) => response(&state, service.status().await),
        Err((status, code)) => error(status, code),
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Request {
    instance_id: Uuid,
    server_url: Option<String>,
    name: Option<String>,
}
async fn change(state: Arc<StateData>, auth: Auth, body: Bytes, action: &str) -> Response {
    let service = match service(&state, &auth) {
        Ok(service) => service,
        Err((status, code)) => return error(status, code),
    };
    if !origin(&auth, &state) {
        return error(StatusCode::FORBIDDEN, "origin_required");
    }
    if auth.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) != Some("application/json")
        || body.len() > 4096
    {
        return error(StatusCode::BAD_REQUEST, "invalid_relay_request");
    }
    let Ok(request) = serde_json::from_slice::<Request>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_relay_request");
    };
    if request.instance_id != state.id {
        return error(StatusCode::CONFLICT, "stale_instance");
    }
    let result = match action {
        "bind" => {
            let (Some(url), Some(name)) = (request.server_url, request.name) else {
                return error(StatusCode::BAD_REQUEST, "invalid_relay_request");
            };
            service.bind(url, name, false).await
        }
        "enable" => service.enable().await,
        "disable" => service.disable().await,
        "forget" => service.forget().await,
        _ => unreachable!(),
    };
    match result {
        Ok(relay) => response(&state, relay),
        Err(_) => error(StatusCode::UNPROCESSABLE_ENTITY, "relay_action_failed"),
    }
}
pub(super) async fn bind(State(state): State<Arc<StateData>>, auth: Auth, body: Bytes) -> Response {
    change(state, auth, body, "bind").await
}
pub(super) async fn enable(
    State(state): State<Arc<StateData>>,
    auth: Auth,
    body: Bytes,
) -> Response {
    change(state, auth, body, "enable").await
}
pub(super) async fn disable(
    State(state): State<Arc<StateData>>,
    auth: Auth,
    body: Bytes,
) -> Response {
    change(state, auth, body, "disable").await
}
pub(super) async fn forget(
    State(state): State<Arc<StateData>>,
    auth: Auth,
    body: Bytes,
) -> Response {
    change(state, auth, body, "forget").await
}
