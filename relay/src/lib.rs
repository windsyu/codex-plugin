mod admin;
mod auth;
mod portal;
mod routing;
mod store;
mod tunnel;

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
pub use store::Store;
use tokio::sync::Semaphore;
use uuid::Uuid;

#[derive(Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub portal_origin: String,
    /// Compatibility option; routing is always single-origin.
    pub single_origin: bool,
    pub password_workers: usize,
    pub allow_insecure_loopback: bool,
    /// Explicit header overwritten by a trusted loopback TLS proxy. Never enable for arbitrary upstreams.
    pub trusted_client_ip_header: Option<String>,
}
impl Config {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=4).contains(&self.password_workers),
            "password-workers must be between 1 and 4"
        );
        if let Some(header) = &self.trusted_client_ip_header {
            anyhow::ensure!(
                header == "x-relay-client-ip",
                "only x-relay-client-ip is supported as an explicitly trusted proxy header"
            );
        }
        let u = url::Url::parse(&self.portal_origin)?;
        anyhow::ensure!(
            u.origin().ascii_serialization() == self.portal_origin
                && u.username().is_empty()
                && u.password().is_none(),
            "portal-origin must be an exact origin"
        );
        let loopback = |h: &str| {
            h == "localhost"
                || h.ends_with(".localhost")
                || h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        };
        anyhow::ensure!(
            u.scheme() == "https"
                || (self.allow_insecure_loopback
                    && u.scheme() == "http"
                    && loopback(u.host_str().unwrap_or("").trim_matches(['[', ']']))),
            "HTTPS is required except explicit isolated loopback testing"
        );
        Ok(())
    }
    pub fn gateway_origin(&self, _id: Uuid) -> Option<String> {
        Some(self.portal_origin.clone())
    }
    fn host_origin(&self, h: &HeaderMap) -> Option<String> {
        let host = h.get("host")?.to_str().ok()?;
        let portal = url::Url::parse(&self.portal_origin).ok()?;
        (format!("{}://{}", portal.scheme(), host) == self.portal_origin)
            .then(|| self.portal_origin.clone())
    }
}
#[derive(Clone)]
pub(crate) struct App(pub Arc<Inner>);
pub(crate) struct Inner {
    config: Config,
    store: Store,
    passwords: Arc<Semaphore>,
    portal_capacity: Arc<Semaphore>,
    control_capacity: Arc<Semaphore>,
    gateway_control_capacity: Mutex<HashMap<Uuid, std::sync::Weak<Semaphore>>>,
    gateway_channel_capacity: Mutex<HashMap<Uuid, std::sync::Weak<Semaphore>>>,
    stream_capacity: Arc<Semaphore>,
    limits: Mutex<HashMap<String, (Instant, u32)>>,
    bindings: Mutex<HashMap<Uuid, portal::Pending>>,
    tunnels: Mutex<HashMap<Uuid, tunnel::Connection>>,
    streams: Mutex<HashMap<Uuid, tunnel::Stream>>,
    #[cfg(test)]
    admission_gate: Mutex<Option<tunnel::AdmissionGate>>,
    #[cfg(test)]
    forget_gate: Mutex<Option<portal::ForgetGate>>,
}
pub fn app(config: Config) -> anyhow::Result<Router> {
    config.validate()?;
    let store = Store::open(&config.data_dir)?;
    let password_workers = config.password_workers;
    let state = App(Arc::new(Inner {
        store,
        config,
        passwords: Arc::new(Semaphore::new(password_workers)),
        portal_capacity: Arc::new(Semaphore::new(32)),
        control_capacity: Arc::new(Semaphore::new(256)),
        gateway_control_capacity: Mutex::new(HashMap::new()),
        gateway_channel_capacity: Mutex::new(HashMap::new()),
        stream_capacity: Arc::new(Semaphore::new(256)),
        limits: Mutex::new(HashMap::new()),
        bindings: Mutex::new(HashMap::new()),
        tunnels: Mutex::new(HashMap::new()),
        streams: Mutex::new(HashMap::new()),
        #[cfg(test)]
        admission_gate: Mutex::new(None),
        #[cfg(test)]
        forget_gate: Mutex::new(None),
    }));
    Ok(router(state))
}
fn router(state: App) -> Router {
    Router::new()
        .route("/", get(portal::index))
        .route("/login", get(portal::index))
        .route("/computers", get(portal::index))
        .route("/authorize", get(portal::index))
        .route("/account/security", get(portal::index))
        .route("/admin/users", get(portal::index))
        .route("/relay.js", get(portal::script))
        .route("/relay.css", get(portal::style))
        .route("/relay/v1/account", get(auth::account))
        .route(
            "/relay/v1/admin/users",
            get(admin::list_users).post(admin::create_user),
        )
        .route(
            "/relay/v1/admin/users/{id}/password",
            post(admin::reset_password),
        )
        .route("/relay/v1/admin/users/{id}", delete(admin::delete_user))
        .route(
            "/relay/v1/admin/users/{id}/disable",
            post(admin::disable_user),
        )
        .route(
            "/relay/v1/admin/users/{id}/enable",
            post(admin::enable_user),
        )
        .route("/relay/v1/login", post(auth::login))
        .route("/relay/v1/password", post(auth::change_password))
        .route("/relay/v1/logout", post(auth::logout))
        .route("/relay/v1/bindings", post(portal::binding_start))
        .route("/relay/v1/bindings/{id}/poll", post(portal::binding_poll))
        .route("/relay/v1/bindings/approve", post(portal::binding_approve))
        .route("/relay/v1/bindings/preview", post(portal::binding_preview))
        .route("/relay/v1/gateways", get(portal::gateways))
        .route("/relay/v1/gateways/{id}", delete(portal::forget))
        .route("/relay/v1/gateways/{id}/open", post(portal::open))
        .route("/relay/v1/gateway/revoke", post(portal::revoke_credential))
        .route("/relay/v1/gateways/{id}/access", get(tunnel::access))
        .route("/relay/v1/tunnel", get(tunnel::connect))
        .route("/relay/v1/tunnel/channels/{id}", get(tunnel::attach))
        .route("/g/{id}/", get(routing::page))
        .route("/g/{id}/history", get(routing::page))
        .route("/g/{id}/launch", get(routing::page))
        .route("/g/{id}/settings", get(routing::page))
        .route("/g/{id}/runs/{run}/{page}", get(routing::page))
        .route("/assets/{*path}", get(routing::asset))
        .route(
            "/g/{id}/workbench/v1/secure-channel",
            get(tunnel::secure_channel),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            codex_relay_protocol::MAX_HTTP_BODY,
        ))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}
pub async fn serve(config: Config, bind: SocketAddr) -> anyhow::Result<()> {
    anyhow::ensure!(
        bind.ip().is_loopback(),
        "relay binds only loopback; terminate TLS at the configured reverse proxy"
    );
    let router = app(config)?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
async fn guard(State(s): State<App>, req: Request, next: Next) -> Response {
    if req.headers().get_all("host").iter().count() != 1
        || req.headers().get_all("origin").iter().count() > 1
        || req.headers().get_all("if-match").iter().count() > 1
        || (req.headers().contains_key("origin")
            && req
                .headers()
                .get("origin")
                .is_none_or(|v| v.to_str().is_err()))
        || req.uri().scheme().is_some()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(origin) = s.0.config.host_origin(req.headers()) else {
        return StatusCode::MISDIRECTED_REQUEST.into_response();
    };
    if s.client_ip(
        req.headers(),
        req.extensions()
            .get::<axum::extract::ConnectInfo<SocketAddr>>()
            .map(|p| p.0),
    )
    .is_err()
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let path = req.uri().path();
    let machine = path == "/relay/v1/bindings"
        || path == "/relay/v1/gateway/revoke"
        || (path.starts_with("/relay/v1/bindings/") && path.ends_with("/poll"))
        || path.starts_with("/relay/v1/tunnel");
    let write = !matches!(req.method().as_str(), "GET" | "HEAD");
    let websocket = req.headers().contains_key("sec-websocket-key");
    let provided = req.headers().get("origin").and_then(|v| v.to_str().ok());
    if ((write || websocket) && !machine && provided != Some(origin.as_str()))
        || provided.is_some_and(|o| o != origin)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if (path.starts_with("/g/") || path.ends_with("/access"))
        && req.headers().keys().any(|n| {
            n.as_str() == "authorization"
                || (n.as_str().starts_with("x-relay-")
                    && s.0.config.trusted_client_ip_header.as_deref() != Some(n.as_str()))
                || n.as_str().starts_with("x-codex-")
                || n.as_str() == "forwarded"
                || n.as_str().starts_with("x-forwarded-")
        })
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let (_portal_permit, req) = if write && path.starts_with("/relay/v1/") {
        let Ok(permit) = s.0.portal_capacity.clone().try_acquire_owned() else {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        };
        let (parts, body) = req.into_parts();
        let body =
            match tokio::time::timeout(Duration::from_secs(10), axum::body::to_bytes(body, 8192))
                .await
            {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(_)) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
                Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
            };
        (
            Some(permit),
            Request::from_parts(parts, axum::body::Body::from(body)),
        )
    } else {
        (None, req)
    };
    let mut r = next.run(req).await;
    r.headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    r.headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    r.headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    r.headers_mut()
        .insert("x-frame-options", "DENY".parse().unwrap());
    r.headers_mut().insert("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; worker-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    r
}
pub(crate) type Result<T> = std::result::Result<T, Failure>;
#[derive(Debug)]
pub(crate) struct Failure(pub StatusCode);
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (
            self.0,
            axum::Json(serde_json::json!({"error":"request_failed"})),
        )
            .into_response()
    }
}
impl From<rusqlite::Error> for Failure {
    fn from(_: rusqlite::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR)
    }
}
pub(crate) fn denied() -> Failure {
    Failure(StatusCode::UNAUTHORIZED)
}
pub(crate) fn bad() -> Failure {
    Failure(StatusCode::BAD_REQUEST)
}
pub(crate) fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    let mut found = None;
    for v in h.get_all("cookie") {
        for pair in v.to_str().ok()?.split(';') {
            let (k, v) = pair.trim().split_once('=')?;
            if k == name {
                if found.is_some() {
                    return None;
                }
                found = Some(v.to_string());
            }
        }
    }
    found
}
impl App {
    pub(crate) fn client_ip(&self, h: &HeaderMap, peer: Option<SocketAddr>) -> Result<String> {
        if let Some(name) = &self.0.config.trusted_client_ip_header {
            if !peer.is_some_and(|p| p.ip().is_loopback()) {
                return Err(bad());
            }
            let mut values = h.get_all(name).iter();
            let ip = values
                .next()
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<IpAddr>().ok())
                .ok_or_else(bad)?;
            if values.next().is_some() {
                return Err(bad());
            }
            Ok(ip.to_string())
        } else {
            Ok(peer
                .map(|p| p.ip().to_string())
                .unwrap_or_else(|| "unknown".into()))
        }
    }
    pub(crate) fn portal_session(&self, h: &HeaderMap) -> Result<store::Session> {
        self.0
            .store
            .session(&cookie(h, "__Host-relay_session").ok_or_else(denied)?, None)
            .ok_or_else(denied)
    }
    pub(crate) fn authorized_portal_session(&self, h: &HeaderMap) -> Result<store::Session> {
        let session = self.portal_session(h)?;
        if session.must_change_password {
            return Err(denied());
        }
        Ok(session)
    }
    pub(crate) fn rate(&self, key: String, max: u32) -> Result<()> {
        let mut limits = self.0.limits.lock().unwrap();
        limits.retain(|_, (t, _)| t.elapsed() < Duration::from_secs(60));
        if limits.len() >= 10000 && !limits.contains_key(&key) {
            return Err(Failure(StatusCode::TOO_MANY_REQUESTS));
        }
        let entry = limits.entry(key).or_insert((Instant::now(), 0));
        entry.1 += 1;
        if entry.1 > max {
            Err(Failure(StatusCode::TOO_MANY_REQUESTS))
        } else {
            Ok(())
        }
    }
    pub(crate) fn revoke_invalid(&self) {
        let mut streams = self.0.streams.lock().unwrap();
        let mut sessions = std::collections::HashSet::new();
        streams.retain(|_, v| {
            if !self.0.store.active(v.session) {
                v.cancel.send_replace(true);
                sessions.insert((v.gateway, v.session));
                false
            } else {
                true
            }
        });
        drop(streams);
        let mut tunnels = self.0.tunnels.lock().unwrap();
        tunnels.retain(|gateway, c| {
            if self
                .0
                .store
                .gateway_auth(&c.credential)
                .is_none_or(|(id, _)| id != *gateway)
            {
                c.cancel.send_replace(true);
                false
            } else {
                true
            }
        });
        for (gateway, c) in tunnels.iter_mut() {
            c.sessions.retain(|session| {
                if !self.0.store.active(*session) {
                    sessions.insert((*gateway, *session));
                    false
                } else {
                    true
                }
            });
        }
        for (gateway, session) in sessions {
            if let Some(c) = tunnels.get(&gateway)
                && c.send
                    .try_send(codex_relay_protocol::ToGateway::Revoke {
                        session_id: session,
                    })
                    .is_err()
            {
                c.cancel.send_replace(true);
            }
        }
    }
}

#[cfg(test)]
mod tests;
