//! One optional device listener and one authorization table for all addresses.
use super::*;
use crate::workbench::permission::Permission;
use serde::Serialize;
use serde_json::json;
use std::future::IntoFuture;
use std::sync::Mutex;
use std::time::Instant;

mod discovery;
const MAX_DEVICES: usize = 8;

#[derive(Clone, Copy)]
pub(super) struct Ingress(pub u64);
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Address {
    pub id: String,
    pub label: String,
    pub origin: String,
}
struct Device {
    digest: String,
    permission: Permission,
    label: String,
}
struct Data {
    revision: u64,
    generation: u64,
    phase: &'static str,
    port: Option<u16>,
    addresses: Vec<Address>,
    notice: Option<&'static str>,
    error: Option<&'static str>,
    devices: Vec<Device>,
    next_label: u64,
    stop: Option<watch::Sender<bool>>,
    attempts: usize,
    window: Instant,
}
pub(super) struct Access {
    // Run-scoped capability, kept only in memory so a local page refresh can
    // retrieve the same QR. Device generations still revoke browser sessions.
    pairing: String,
    pairing_id: Uuid,
    data: Mutex<Data>,
    #[cfg(test)]
    discovery: Mutex<Option<discovery::Found>>,
}
impl Access {
    pub fn new() -> Self {
        Self {
            pairing: secret(),
            pairing_id: Uuid::new_v4(),
            data: Mutex::new(Data {
                revision: 1,
                generation: 0,
                phase: "off",
                port: None,
                addresses: vec![],
                notice: None,
                error: None,
                devices: vec![],
                next_label: 1,
                stop: None,
                attempts: 0,
                window: Instant::now(),
            }),
            #[cfg(test)]
            discovery: Mutex::new(None),
        }
    }
    pub fn valid_host(&self, host: &str) -> bool {
        let d = self.data.lock().unwrap();
        d.phase == "ready"
            && d.addresses
                .iter()
                .any(|a| a.origin.strip_prefix("http://") == Some(host))
    }
    pub fn valid_ingress(&self, generation: u64, host: &str) -> bool {
        let d = self.data.lock().unwrap();
        d.phase == "ready"
            && d.generation == generation
            && d.addresses
                .iter()
                .any(|a| a.origin.strip_prefix("http://") == Some(host))
    }
    fn cookie(&self, epoch: Uuid) -> String {
        format!(
            "workbench_device_{}_{}",
            epoch.simple(),
            self.data.lock().unwrap().generation
        )
    }
    pub fn permission(&self, headers: &HeaderMap, epoch: Uuid) -> Option<Permission> {
        let d = self.data.lock().unwrap();
        if d.phase != "ready" {
            return None;
        }
        let name = format!("workbench_device_{}_{}", epoch.simple(), d.generation);
        let token = cookie_value(headers, &name)?;
        let digest = digest(token);
        d.devices
            .iter()
            .find(|v| v.permission.active() && matches_secret(&digest, &v.digest))
            .map(|v| v.permission.clone())
    }
    fn view(&self, epoch: Uuid) -> serde_json::Value {
        let d = self.data.lock().unwrap();
        json!({"runEpoch":epoch,"revision":d.revision,"state":d.phase,"port":d.port,"addresses":d.addresses,
            "notice":d.notice,"error":d.error,"pairingId":self.pairing_id,
            "devices":d.devices.iter().map(|v| json!({"id":v.permission.id(),"label":v.label})).collect::<Vec<_>>()})
    }
    fn invalidate(d: &mut Data) {
        for device in d.devices.drain(..) {
            device.permission.revoke();
        }
    }
    fn finish(&self, generation: u64, error: Option<&'static str>) {
        let mut d = self.data.lock().unwrap();
        if d.generation != generation {
            return;
        }
        Self::invalidate(&mut d);
        d.phase = if error.is_some() { "error" } else { "off" };
        d.error = error;
        d.port = None;
        d.addresses.clear();
        d.stop = None;
        d.revision += 1;
    }
    async fn discover(&self) -> discovery::Found {
        #[cfg(test)]
        if let Some(found) = self.discovery.lock().unwrap().clone() {
            return found;
        }
        discovery::discover().await
    }
}
fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex().to_string()
}
fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|v| v.trim().split_once('='))
        .find_map(|(key, value)| (key == name && value.len() == 64).then_some(value))
}
fn failure(state: &WebState, status: StatusCode, code: &'static str) -> Response {
    (
        status,
        Json(
            json!({"error":{"source":"workbench_access","runEpoch":state.hub.epoch(),"code":code}}),
        ),
    )
        .into_response()
}
fn admin(state: &WebState, headers: &HeaderMap) -> Result<(), (StatusCode, &'static str)> {
    if !owner_authorised(headers, state) {
        return Err((StatusCode::FORBIDDEN, "local_management_required"));
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RunRequest {
    run_epoch: Uuid,
}
fn revision(
    state: &WebState,
    headers: &HeaderMap,
    epoch: Uuid,
    d: &Data,
) -> Result<(), (StatusCode, &'static str)> {
    admin(state, headers)?;
    if !same_origin(headers, state) {
        return Err((StatusCode::FORBIDDEN, "origin_required"));
    }
    if epoch != state.hub.epoch() {
        return Err((StatusCode::CONFLICT, "stale_run"));
    }
    let values = headers.get_all(header::IF_MATCH);
    let Some(value) = values.iter().next().and_then(|v| v.to_str().ok()) else {
        return Err((StatusCode::PRECONDITION_REQUIRED, "revision_required"));
    };
    if values.iter().count() != 1 || value != format!("\"{}\"", d.revision) {
        return Err((StatusCode::PRECONDITION_FAILED, "access_changed"));
    }
    Ok(())
}
pub(super) async fn status(State(state): State<Arc<WebState>>, headers: HeaderMap) -> Response {
    if let Err((status, code)) = admin(&state, &headers) {
        return failure(&state, status, code);
    }
    Json(state.access.view(state.hub.epoch())).into_response()
}
pub(super) async fn enable(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(request) = serde_json::from_slice::<RunRequest>(&body) else {
        return failure(&state, StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    };
    let (generation, stop) = {
        let mut d = state.access.data.lock().unwrap();
        if let Err((status, code)) = revision(&state, &headers, request.run_epoch, &d) {
            return failure(&state, status, code);
        }
        if !matches!(d.phase, "off" | "error") {
            return failure(&state, StatusCode::CONFLICT, "access_busy");
        }
        let (tx, rx) = watch::channel(false);
        d.generation += 1;
        d.revision += 1;
        d.phase = "starting";
        d.error = None;
        d.stop = Some(tx);
        (d.generation, rx)
    };
    let running = state.clone();
    tokio::spawn(async move {
        serve_devices(running, generation, stop).await;
    });
    (
        StatusCode::ACCEPTED,
        Json(state.access.view(state.hub.epoch())),
    )
        .into_response()
}
async fn serve_devices(state: Arc<WebState>, generation: u64, mut stop: watch::Receiver<bool>) {
    let mut global_stop = state.stop.clone();
    let task = async {
        let port = if let Some(settings) = &state.options.settings {
            let info = settings.read().await.map_err(|_| "config_unavailable")?;
            if !info.errors.is_empty() {
                return Err("config_unavailable");
            }
            info.saved
                .and_then(|c| c.access)
                .map(|a| a.port)
                .unwrap_or(0)
        } else {
            0
        };
        #[cfg(not(test))]
        let bind = Ipv4Addr::UNSPECIFIED;
        // API fixtures need no exposure to real network interfaces.
        #[cfg(test)]
        let bind = if state.access.discovery.lock().unwrap().is_some() {
            Ipv4Addr::LOCALHOST
        } else {
            Ipv4Addr::UNSPECIFIED
        };
        let listener = TcpListener::bind((bind, port))
            .await
            .map_err(|_| "listen_failed")?;
        let port = listener.local_addr().map_err(|_| "listen_failed")?.port();
        let found = state.access.discover().await;
        {
            let mut d = state.access.data.lock().unwrap();
            if d.generation != generation || d.phase != "starting" {
                return Ok(());
            }
            d.addresses = found.addresses(port);
            d.notice = found.notice;
            d.port = Some(port);
            d.phase = "ready";
            d.revision += 1;
        }
        let app = router(state.clone()).layer(axum::Extension(Ingress(generation)));
        let server = axum::serve(listener, app).into_future();
        tokio::pin!(server);
        let refresh = async {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let found = state.access.discover().await;
                let mut d = state.access.data.lock().unwrap();
                if d.generation != generation || d.phase != "ready" {
                    return;
                }
                let addresses = found.addresses(port);
                if d.addresses != addresses || d.notice != found.notice {
                    d.addresses = addresses;
                    d.notice = found.notice;
                    d.revision += 1;
                }
            }
        };
        tokio::select! {
            _ = &mut server => Err("listener_ended"),
            _ = refresh => Ok(()),
        }
    };
    let result = tokio::select! {
        biased;
        _ = stop.wait_for(|v| *v) => Ok(()),
        _ = global_stop.wait_for(|v| *v) => Ok(()),
        result = task => result,
    };
    state.access.finish(generation, result.err());
}
pub(super) async fn pairing(State(state): State<Arc<WebState>>, headers: HeaderMap) -> Response {
    if let Err((status, code)) = admin(&state, &headers) {
        return failure(&state, status, code);
    }
    let d = state.access.data.lock().unwrap();
    if d.phase != "ready" || d.addresses.is_empty() {
        return failure(&state, StatusCode::CONFLICT, "no_device_address");
    }
    let links: Vec<_> = d
        .addresses
        .iter()
        .map(|a| {
            json!({
                "addressId":a.id,"url":format!("{}/#pair={}", a.origin, state.access.pairing)
            })
        })
        .collect();
    Json(json!({"runEpoch":state.hub.epoch(),"revision":d.revision,"pairingId":state.access.pairing_id,"links":links})).into_response()
}
pub(super) fn pair_device(state: &WebState, token: &str, headers: &HeaderMap) -> Response {
    let cookie = state.access.cookie(state.hub.epoch());
    let mut d = state.access.data.lock().unwrap();
    if d.phase != "ready" {
        return failure(state, StatusCode::GONE, "access_closed");
    }
    if d.window.elapsed() >= Duration::from_secs(60) {
        d.window = Instant::now();
        d.attempts = 0;
    }
    if d.attempts >= 64 {
        return failure(state, StatusCode::TOO_MANY_REQUESTS, "pairing_rate_limit");
    }
    d.attempts += 1;
    if !matches_secret(token, &state.access.pairing) {
        return failure(state, StatusCode::GONE, "pairing_invalid");
    }
    // A repeat scan from an authorised browser must preserve its session and
    // terminal reconnect grant, even when all browser slots are occupied.
    if let Some(existing) = cookie_value(headers, &cookie) {
        let existing = digest(existing);
        if d.devices
            .iter()
            .any(|v| v.permission.active() && matches_secret(&existing, &v.digest))
        {
            return StatusCode::NO_CONTENT.into_response();
        }
    }
    if d.devices.len() >= MAX_DEVICES {
        return failure(state, StatusCode::CONFLICT, "device_limit");
    }
    let token = secret();
    let permission = Permission::new();
    let agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let kind = if agent.contains("iPhone") || agent.contains("Android") {
        "手机浏览器"
    } else if agent.contains("iPad") {
        "平板浏览器"
    } else {
        "浏览器"
    };
    let label = format!("{kind} {}", d.next_label);
    d.next_label += 1;
    d.devices.push(Device {
        digest: digest(&token),
        permission,
        label,
    });
    d.revision += 1;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{cookie}={token}; HttpOnly; SameSite=Strict; Path=/"
        ))
        .unwrap(),
    );
    response
}
async fn terminal_barrier(state: &WebState) -> bool {
    let Some(t) = &state.options.terminal else {
        return true;
    };
    for _ in 0..20 {
        if matches!(
            tokio::time::timeout(Duration::from_millis(250), t.revoke_access()).await,
            Ok(Ok(()))
        ) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}
pub(super) async fn disable(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(request) = serde_json::from_slice::<RunRequest>(&body) else {
        return failure(&state, StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    };
    {
        let mut d = state.access.data.lock().unwrap();
        if let Err((status, code)) = revision(&state, &headers, request.run_epoch, &d) {
            return failure(&state, status, code);
        }
        Access::invalidate(&mut d);
        if let Some(stop) = &d.stop {
            stop.send_replace(true);
            d.phase = "stopping";
        } else {
            d.phase = "off";
        }
        d.revision += 1;
    }
    if !terminal_barrier(&state).await {
        return failure(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            "revocation_pending",
        );
    }
    (
        StatusCode::ACCEPTED,
        Json(state.access.view(state.hub.epoch())),
    )
        .into_response()
}
pub(super) async fn revoke(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(request): Query<RunRequest>,
) -> Response {
    {
        let mut d = state.access.data.lock().unwrap();
        if let Err((status, code)) = revision(&state, &headers, request.run_epoch, &d) {
            return failure(&state, status, code);
        }
        d.devices.retain(|device| {
            if device.permission.id() == id {
                device.permission.revoke();
                false
            } else {
                true
            }
        });
        d.revision += 1;
    }
    if !terminal_barrier(&state).await {
        return failure(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            "revocation_pending",
        );
    }
    Json(state.access.view(state.hub.epoch())).into_response()
}

#[cfg(test)]
mod tests;
