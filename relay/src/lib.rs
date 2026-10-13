mod auth;
mod portal;
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
    pub workbench_domain: String,
    pub workbench_origin: Option<String>,
    pub fixed_gateway_id: Option<Uuid>,
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
        if let Some(origin) = &self.workbench_origin {
            anyhow::ensure!(
                self.workbench_domain.is_empty(),
                "workbench-origin conflicts with workbench-domain"
            );
            anyhow::ensure!(
                self.fixed_gateway_id.is_some_and(|id| !id.is_nil()),
                "fixed-gateway-id is required for workbench-origin"
            );
            let workbench = url::Url::parse(origin)?;
            let ip = |url: &url::Url| {
                url.host_str()
                    .and_then(|h| h.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            };
            anyhow::ensure!(
                workbench.origin().ascii_serialization() == *origin
                    && workbench.username().is_empty()
                    && workbench.password().is_none(),
                "workbench-origin must be an exact origin"
            );
            anyhow::ensure!(
                ip(&u).is_some() && ip(&u) == ip(&workbench),
                "fixed origins must use the same literal IP"
            );
            anyhow::ensure!(
                u.port().is_some() && workbench.port().is_some() && u.port() != workbench.port(),
                "fixed origins require distinct explicit non-default ports"
            );
            anyhow::ensure!(
                u.scheme() == workbench.scheme()
                    && (u.scheme() == "https"
                        || (self.allow_insecure_loopback
                            && u.scheme() == "http"
                            && ip(&u).is_some_and(|ip| ip.is_loopback()))),
                "HTTPS is required except explicit isolated loopback testing"
            );
            return Ok(());
        }
        anyhow::ensure!(
            self.fixed_gateway_id.is_none(),
            "fixed-gateway-id requires workbench-origin"
        );
        anyhow::ensure!(
            u.scheme() == "https"
                || (self.allow_insecure_loopback
                    && u.scheme() == "http"
                    && loopback(u.host_str().unwrap_or(""))
                    && loopback(&self.workbench_domain)),
            "HTTPS is required except explicit isolated loopback testing"
        );
        anyhow::ensure!(
            !self.workbench_domain.is_empty()
                && self
                    .workbench_domain
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
                && !self.workbench_domain.starts_with('.')
                && !self.workbench_domain.ends_with('.'),
            "invalid workbench domain"
        );
        anyhow::ensure!(
            !u.host_str()
                .unwrap_or("")
                .ends_with(&format!(".{}", self.workbench_domain))
                || !u.host_str().unwrap_or("").starts_with("g-"),
            "portal must have a separate origin"
        );
        Ok(())
    }
    pub fn gateway_origin(&self, id: Uuid) -> Option<String> {
        if let Some(origin) = &self.workbench_origin {
            return (self.fixed_gateway_id == Some(id)).then(|| origin.clone());
        }
        let u = url::Url::parse(&self.portal_origin).expect("validated config");
        Some(format!(
            "{}://g-{}.{}{}",
            u.scheme(),
            id,
            self.workbench_domain,
            u.port().map(|p| format!(":{p}")).unwrap_or_default()
        ))
    }
    fn host_origin(&self, h: &HeaderMap) -> Option<(String, Option<Uuid>)> {
        let host = h.get("host")?.to_str().ok()?;
        let portal = url::Url::parse(&self.portal_origin).ok()?;
        if format!("{}://{}", portal.scheme(), host) == self.portal_origin {
            return Some((self.portal_origin.clone(), None));
        }
        if let Some(origin) = &self.workbench_origin {
            return (format!("{}://{}", portal.scheme(), host) == *origin)
                .then(|| (origin.clone(), self.fixed_gateway_id));
        }
        let bare = host.split(':').next()?;
        let id = bare
            .strip_suffix(&format!(".{}", self.workbench_domain))?
            .strip_prefix("g-")?;
        let id = Uuid::parse_str(id).ok()?;
        let origin = self.gateway_origin(id)?;
        (format!("{}://{}", portal.scheme(), host) == origin).then_some((origin, Some(id)))
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
    stream_capacity: Arc<Semaphore>,
    terminal_capacity: Arc<Semaphore>,
    limits: Mutex<HashMap<String, (Instant, u32)>>,
    bindings: Mutex<HashMap<Uuid, portal::Pending>>,
    tunnels: Mutex<HashMap<Uuid, tunnel::Connection>>,
    streams: Mutex<HashMap<Uuid, tunnel::Stream>>,
    #[cfg(test)]
    admission_gate: Mutex<Option<tunnel::AdmissionGate>>,
}
pub fn app(config: Config) -> anyhow::Result<Router> {
    config.validate()?;
    let store = Store::open(&config.data_dir)?;
    if let Some(id) = config.fixed_gateway_id {
        let incompatible: bool = store.0.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM gateways WHERE id != ?1)",
            [id.to_string()],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            !incompatible,
            "stored gateways conflict with fixed-gateway-id"
        );
    }
    let password_workers = config.password_workers;
    let state = App(Arc::new(Inner {
        store,
        config,
        passwords: Arc::new(Semaphore::new(password_workers)),
        portal_capacity: Arc::new(Semaphore::new(32)),
        control_capacity: Arc::new(Semaphore::new(256)),
        gateway_control_capacity: Mutex::new(HashMap::new()),
        stream_capacity: Arc::new(Semaphore::new(256)),
        terminal_capacity: Arc::new(Semaphore::new(16)),
        limits: Mutex::new(HashMap::new()),
        bindings: Mutex::new(HashMap::new()),
        tunnels: Mutex::new(HashMap::new()),
        streams: Mutex::new(HashMap::new()),
        #[cfg(test)]
        admission_gate: Mutex::new(None),
    }));
    Ok(Router::new()
        .route("/", get(portal::index))
        .route("/relay.js", get(portal::script))
        .route("/relay/v1/account", get(auth::account))
        .route("/relay/v1/login", post(auth::login))
        .route("/relay/v1/register", post(auth::redeem))
        .route("/relay/v1/reset", post(auth::redeem))
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
        .route("/relay/v1/open", get(portal::exchange_page))
        .route("/relay/v1/exchange.js", get(portal::exchange_script))
        .route("/relay/v1/session/exchange", post(portal::exchange))
        .route("/relay/v1/tunnel", get(tunnel::connect))
        .route("/relay/v1/tunnel/streams/{id}", get(tunnel::attach))
        .fallback(tunnel::forward)
        .layer(axum::extract::DefaultBodyLimit::max(
            codex_relay_protocol::MAX_HTTP_BODY,
        ))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state))
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
    let Some((origin, gateway)) = s.0.config.host_origin(req.headers()) else {
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
    let gateway_route = path == "/"
        || (!path.starts_with("/relay") && gateway.is_some())
        || matches!(
            path,
            "/relay/v1/open" | "/relay/v1/exchange.js" | "/relay/v1/session/exchange"
        );
    if (gateway.is_some() && !gateway_route)
        || (gateway.is_none()
            && matches!(
                path,
                "/relay/v1/open" | "/relay/v1/exchange.js" | "/relay/v1/session/exchange"
            ))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
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
    if gateway.is_some()
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
    r.headers_mut().entry("content-security-policy").or_insert("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    r
}
pub(crate) type Result<T> = std::result::Result<T, Failure>;
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
