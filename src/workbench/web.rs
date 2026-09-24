//! Shared workbench surface with an optional, explicitly enabled device listener.
//! The model route capability
//! is never exposed here; browser pairing uses an independent per-run secret.

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{Html, IntoResponse, Response, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use uuid::Uuid;

use super::live::{LiveHub, SubscribeError};

#[cfg(unix)]
mod access;
mod details_api;
mod history_api;
#[cfg(unix)]
mod management_api;
#[cfg(unix)]
mod settings_api;
#[cfg(unix)]
mod terminal_api;
#[cfg(unix)]
mod workspace_api;

#[derive(Default)]
struct RuntimeOptions {
    #[cfg(unix)]
    workspace: Option<super::workspace::Handle>,
    #[cfg(unix)]
    terminal: Option<super::terminal::TerminalHandle>,
    #[cfg(unix)]
    settings: Option<super::config::ConfigHandle>,
    #[cfg(unix)]
    management: Option<super::recording::management::Handle>,
}

const COOKIE: &str = "workbench_session";
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";
// xterm positions its canvas/rows through inline styles. Scripts remain self-only.
const WORKBENCH_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

#[derive(rust_embed::RustEmbed)]
#[folder = "web/dist"]
struct WorkbenchAssets;

impl WebState {
    fn has_terminal(&self) -> bool {
        #[cfg(unix)]
        {
            self.options.terminal.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

struct WebState {
    hub: Arc<LiveHub>,
    authority: String,
    origin: String,
    pairing: String,
    cookie_name: String,
    session: String,
    snapshot_slots: Arc<Semaphore>,
    stop: watch::Receiver<bool>,
    options: RuntimeOptions,
    #[cfg(unix)]
    access: access::Access,
    #[cfg(unix)]
    shared_access: Option<Arc<access::SharedAccess>>,
    #[cfg(unix)]
    terminal_slots: Arc<Semaphore>,
}

pub struct ReadingServer {
    address: SocketAddr,
    state: Arc<WebState>,
    stop: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
    #[cfg(unix)]
    _management: Option<super::recording::management::Service>,
}
impl ReadingServer {
    pub async fn bind(hub: Arc<LiveHub>) -> Result<Self> {
        Self::bind_options(hub, RuntimeOptions::default(), None).await
    }
    #[cfg(unix)]
    pub async fn bind_with_terminal(
        hub: Arc<LiveHub>,
        terminal: super::terminal::TerminalHandle,
    ) -> Result<Self> {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        Self::bind_with_terminal_listener(hub, terminal, listener).await
    }
    #[cfg(unix)]
    pub async fn bind_with_terminal_listener(
        hub: Arc<LiveHub>,
        terminal: super::terminal::TerminalHandle,
        listener: std::net::TcpListener,
    ) -> Result<Self> {
        Self::bind_configured(hub, terminal, listener, None, None).await
    }
    #[cfg(unix)]
    pub async fn bind_configured(
        hub: Arc<LiveHub>,
        terminal: super::terminal::TerminalHandle,
        listener: std::net::TcpListener,
        settings: Option<super::config::ConfigHandle>,
        workspace: Option<super::workspace::Handle>,
    ) -> Result<Self> {
        anyhow::ensure!(
            listener.local_addr()?.ip().is_loopback(),
            "workbench listener must be loopback"
        );
        anyhow::ensure!(
            hub.epoch() == terminal.epoch(),
            "terminal belongs to another run"
        );
        anyhow::ensure!(
            WorkbenchAssets::get("workbench.html").is_some(),
            "build the workbench frontend with npm run build in web/ before starting"
        );
        Self::bind_options(
            hub,
            RuntimeOptions {
                terminal: Some(terminal),
                settings,
                management: None,
                workspace,
            },
            Some(listener),
        )
        .await
    }
    async fn bind_options(
        hub: Arc<LiveHub>,
        mut options: RuntimeOptions,
        listener: Option<std::net::TcpListener>,
    ) -> Result<Self> {
        #[cfg(unix)]
        let management = if let (Some(config), Some(history)) = (&options.settings, hub.history()) {
            Some(super::recording::management::Service::start(
                history.root,
                history.workspace,
                hub.epoch(),
                config.clone(),
            )?)
        } else {
            None
        };
        #[cfg(unix)]
        {
            options.management = management.as_ref().map(|s| s.handle());
        }
        let listener = match listener {
            Some(listener) => listener,
            None => std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .context("bind loopback reading surface")?,
        };
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let (stop, stop_rx) = watch::channel(false);
        let state = Arc::new(WebState {
            cookie_name: format!("{COOKIE}_{}", hub.epoch().simple()),
            hub,
            authority: address.to_string(),
            origin: format!("http://{address}"),
            pairing: secret(),
            session: secret(),
            snapshot_slots: Arc::new(Semaphore::new(2)),
            stop: stop_rx.clone(),
            options,
            #[cfg(unix)]
            access: access::Access::new(),
            #[cfg(unix)]
            shared_access: None,
            #[cfg(unix)]
            terminal_slots: Arc::new(Semaphore::new(32)),
        });
        let app = router(state.clone());
        // Reading-page cloning/serialization has its own executor, so a busy
        // page cannot occupy the runtime used for model traffic forwarding.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build reading executor")?;
        let thread = std::thread::Builder::new()
            .name("reading-web".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let listener = TcpListener::from_std(listener)
                        .expect("register loopback reading listener");
                    let mut stop_rx = stop_rx;
                    tokio::select! {
                        biased;
                        _ = stop_rx.changed() => {},
                        _ = async { let _ = axum::serve(listener, app).await; } => {},
                    }
                });
                // Dropping this dedicated runtime closes remaining slow sockets.
            })
            .context("spawn reading executor")?;
        Ok(Self {
            address,
            state,
            stop,
            thread: Some(thread),
            #[cfg(unix)]
            _management: management,
        })
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub(crate) fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
    }
    /// Pairing capability: pass directly to the local browser, never a log.
    pub fn bootstrap_url(&self) -> String {
        format!("{}/#pair={}", self.state.origin, self.state.pairing)
    }
}
impl Drop for ReadingServer {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn router(state: Arc<WebState>) -> Router {
    let app = Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route(
            "/workbench.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("reading/app.js"),
                )
            }),
        )
        .route(
            "/workbench.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("reading/style.css"),
                )
            }),
        )
        .route("/workbench/v1/pair", post(pair))
        .route("/workbench/v1/run", get(run))
        .route("/workbench/v1/history", get(history_api::list))
        .route("/workbench/v1/history/{epoch}", get(history_api::read))
        .route(
            "/workbench/v1/history/{epoch}/status",
            get(history_api::status),
        )
        .route(
            "/workbench/v1/history/{epoch}/requests/{request}",
            get(history_api::details),
        )
        .route(
            "/workbench/v1/requests/{request_id}",
            get(details_api::read),
        )
        .route("/workbench/v1/live/snapshot", get(snapshot))
        .route("/workbench/v1/live/events", get(events));
    #[cfg(unix)]
    let app = app
        .route("/workbench/v1/access", get(access::status))
        .route("/workbench/v1/access/enable", post(access::enable))
        .route("/workbench/v1/access/pairing", get(access::pairing))
        .route("/workbench/v1/access/disable", post(access::disable))
        .route(
            "/workbench/v1/access/devices/{id}",
            axum::routing::delete(access::revoke),
        )
        .route("/workbench/v1/workspace/files", get(workspace_api::read))
        .route("/workbench/v1/workspace/file", get(workspace_api::read))
        .route("/workbench/v1/workspace/search", get(workspace_api::read))
        .route(
            "/workbench/v1/workspace/git/status",
            get(workspace_api::read),
        )
        .route("/workbench/v1/workspace/git/log", get(workspace_api::read))
        .route("/workbench/v1/workspace/git/diff", get(workspace_api::read))
        .route(
            "/workbench/v1/history/cleanup/jobs",
            get(management_api::jobs).post(management_api::create_job),
        )
        .route(
            "/workbench/v1/history/cleanup/jobs/{id}",
            get(management_api::read_job),
        )
        .route(
            "/workbench/v1/history/cleanup/jobs/{id}/cancel",
            post(management_api::cancel),
        )
        .route("/workbench/v1/history/usage", get(management_api::usage))
        .route(
            "/workbench/v1/history/usage/refresh",
            post(management_api::refresh),
        )
        .route(
            "/workbench/v1/history/cleanup/preview",
            post(management_api::preview).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/workbench/v1/history/cleanup/previews/{id}",
            get(management_api::read_preview),
        )
        .route("/workbench/v1/terminal", get(terminal_api::upgrade))
        .route(
            "/workbench/v1/settings",
            get(settings_api::read)
                .put(settings_api::save)
                .layer(DefaultBodyLimit::max(super::config::LIMIT)),
        )
        .route("/workbench/v1/stop", post(terminal_api::stop_run))
        .route("/workbench/v1/run/stop", post(terminal_api::stop_run));
    app.layer(DefaultBodyLimit::max(1024))
        .layer(middleware::from_fn_with_state(state.clone(), boundary))
        .with_state(state.clone())
}

fn secret() -> String {
    rand::random::<[u8; 32]>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn matches_secret(value: &str, expected: &str) -> bool {
    value.len() == expected.len()
        && value
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}
fn authorised(headers: &HeaderMap, state: &WebState) -> bool {
    owner_authorised(headers, state) || remote_permission(headers, state).is_some()
}
fn remote_permission(
    headers: &HeaderMap,
    state: &WebState,
) -> Option<super::permission::Permission> {
    #[cfg(unix)]
    if headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|host| state.access.valid_host(host))
    {
        return state.access.permission(headers, state.hub.epoch());
    }
    None
}
fn same_origin(headers: &HeaderMap, state: &WebState) -> bool {
    if headers.get_all(header::HOST).iter().count() != 1
        || headers.get_all(header::ORIGIN).iter().count() != 1
    {
        return false;
    }
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let known = host == state.authority;
    #[cfg(unix)]
    let known = known || state.access.valid_host(host);
    known
        && headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
            == Some(format!("http://{host}").as_str())
}
fn owner_authorised(headers: &HeaderMap, state: &WebState) -> bool {
    if headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(state.authority.as_str()) {
        return false;
    }
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|entry| entry.trim().split_once('='))
        .any(|(name, value)| name == state.cookie_name && matches_secret(value, &state.session))
}
fn error(status: StatusCode, code: &'static str) -> Response {
    (
        status,
        Json(serde_json::json!({"error":{"source":"workbench_web","code":code}})),
    )
        .into_response()
}

async fn boundary(
    State(state): State<Arc<WebState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    #[cfg(unix)]
    let valid_host = match request.extensions().get::<access::Ingress>() {
        Some(ingress) => state.access.valid_ingress(ingress.0, host),
        None => host == state.authority,
    };
    #[cfg(not(unix))]
    let valid_host = host == state.authority;
    let valid_host = valid_host && headers.get_all(header::HOST).iter().count() == 1;
    let valid_origin = !headers.contains_key(header::ORIGIN) || same_origin(headers, &state);
    let permission = remote_permission(headers, &state);
    let valid_site = headers
        .get("sec-fetch-site")
        .is_none_or(|site| matches!(site.to_str(), Ok("same-origin" | "none")));
    let mut response =
        if !valid_host || !valid_origin || !valid_site || request.uri().scheme().is_some() {
            error(StatusCode::FORBIDDEN, "invalid_origin_or_host")
        } else {
            if let Some(permission) = permission {
                let response = tokio::select! {
                    biased;
                    _ = permission.revoked() => error(StatusCode::UNAUTHORIZED, "access_revoked"),
                    response = next.run(request) => response,
                };
                let (parts, body) = response.into_parts();
                let mut body = body.into_data_stream();
                let stream = async_stream::stream! {
                    loop {
                        let chunk = tokio::select! {
                            biased;
                            _ = permission.revoked() => break,
                            chunk = body.next() => chunk,
                        };
                        let Some(chunk) = chunk else { break; };
                        yield chunk;
                    }
                };
                Response::from_parts(parts, Body::from_stream(stream))
            } else {
                next.run(request).await
            }
        };
    for (name, value) in [
        (header::CACHE_CONTROL, "no-store"),
        (
            header::CONTENT_SECURITY_POLICY,
            if state.has_terminal() {
                WORKBENCH_CSP
            } else {
                CSP
            },
        ),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pair {
    token: String,
}
async fn pair(State(state): State<Arc<WebState>>, headers: HeaderMap, body: Bytes) -> Response {
    if !same_origin(&headers, &state) {
        return error(StatusCode::FORBIDDEN, "origin_required");
    }
    let Ok(pair) = serde_json::from_slice::<Pair>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_pair");
    };
    #[cfg(unix)]
    if headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(state.authority.as_str()) {
        return access::pair_device(&state, &pair.token, &headers);
    }
    if !matches_secret(&pair.token, &state.pairing) {
        return error(StatusCode::FORBIDDEN, "invalid_pair");
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{}={}; HttpOnly; SameSite=Strict; Path=/",
            state.cookie_name, state.session
        ))
        .unwrap(),
    );
    response
}
async fn run(State(state): State<Arc<WebState>>, headers: HeaderMap) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    #[cfg(unix)]
    let terminal = state.options.terminal.is_some();
    #[cfg(not(unix))]
    let terminal = false;
    #[cfg(unix)]
    let settings_available = state.options.settings.is_some() && owner_authorised(&headers, &state);
    #[cfg(not(unix))]
    let settings_available = false;
    #[cfg(unix)]
    let history_management_available = state.options.management.is_some();
    #[cfg(not(unix))]
    let history_management_available = false;
    #[cfg(unix)]
    let (project_name, process_id) = state
        .options
        .terminal
        .as_ref()
        .map(|t| (Some(t.project_name()), Some(t.process_id())))
        .unwrap_or((None, None));
    #[cfg(not(unix))]
    let (project_name, process_id): (Option<&str>, Option<u32>) = (None, None);
    #[cfg(unix)]
    let workspace_root = state.options.workspace.as_ref().map(|h| h.root.as_str());
    #[cfg(not(unix))]
    let workspace_root: Option<&str> = None;
    Json(serde_json::json!({"workspaceRoot":workspace_root,"runEpoch":state.hub.epoch(),"mode":if terminal { "native-workbench" } else { "r0-reading" },"scope":"typed-conversation-items","readingSchemaVersion":super::live::VIEW_SCHEMA_VERSION,"recorder":state.hub.recorder_status().state,"historyAvailable":state.hub.history().is_some(),"historyManagementAvailable":history_management_available,"settingsAvailable":settings_available,"accessAvailable":terminal && owner_authorised(&headers,&state),"terminalAvailable":terminal,"projectName":project_name,"processId":process_id})).into_response()
}

async fn index(State(state): State<Arc<WebState>>) -> Response {
    if state.has_terminal() {
        return match WorkbenchAssets::get("workbench.html") {
            Some(file) => (
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                file.data.into_owned(),
            )
                .into_response(),
            None => error(StatusCode::SERVICE_UNAVAILABLE, "frontend_unavailable"),
        };
    }
    Html(include_str!("reading/index.html")).into_response()
}
async fn asset(Path(path): Path<String>) -> Response {
    if path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let content_type = if path.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match WorkbenchAssets::get(&format!("assets/{path}")) {
        Some(file) => (
            [(header::CONTENT_TYPE, content_type)],
            file.data.into_owned(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn snapshot(State(state): State<Arc<WebState>>, headers: HeaderMap) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    let Ok(permit) = state.snapshot_slots.clone().try_acquire_owned() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "snapshot_busy");
    };
    let (parts, body) = Json(state.hub.snapshot()).into_response().into_parts();
    let stream = async_stream::stream! {
        let _permit = permit;
        let mut body = body.into_data_stream();
        while let Some(bytes) = body.next().await { yield bytes; }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}
#[derive(Deserialize)]
struct Cursor {
    epoch: Uuid,
    after: u64,
}
async fn events(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Query(cursor): Query<Cursor>,
) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    let mut subscription = match state.hub.subscribe(cursor.epoch, cursor.after) {
        Ok(subscription) => subscription,
        Err(SubscribeError::SnapshotRequired) => {
            return error(StatusCode::CONFLICT, "snapshot_required");
        }
        Err(SubscribeError::ClientLimit) => {
            return error(StatusCode::SERVICE_UNAVAILABLE, "reader_limit");
        }
    };
    let mut stop = state.stop.clone();
    let (disabled_sender, mut recorder) = watch::channel(state.hub.recorder_status());
    let recording = state.hub.recorder_updates().is_some();
    if let Some(updates) = state.hub.recorder_updates() {
        recorder = updates;
    }
    let stream = async_stream::stream! {
        let _keep_disabled_open = disabled_sender;
        if recording { yield Ok::<_, Infallible>(Event::default().event("recorder.status").data(serde_json::to_string(&state.hub.recorder_status()).expect("recorder DTO"))); }
        loop {
            let message = tokio::select! {
                value = subscription.recv() => value,
                _ = stop.changed() => return,
                changed = recorder.changed() => {
                    if changed.is_err() { return; }
                    yield Ok(Event::default().event("recorder.status").data(serde_json::to_string(&state.hub.recorder_status()).expect("recorder DTO")));
                    continue;
                }
            };
            let Some(message) = message else { break; };
            yield Ok::<_, Infallible>(Event::default().event("view").id(message.sequence.to_string()).data(message.json.clone()));
        }
        yield Ok(Event::default().event("snapshot_required").data("{}"));
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(10)))
        .into_response()
}

#[cfg(test)]
mod tests;

#[cfg(unix)]
pub(crate) mod application;

/// A Run's routes and optional device listener, without an application listener.
#[cfg(unix)]
pub(crate) struct RunSurface {
    state: Arc<WebState>,
    stop: watch::Sender<bool>,
    _management: Option<super::recording::management::Service>,
}
#[cfg(unix)]
impl RunSurface {
    pub(crate) fn disable_devices(&self) {
        self.state.access.close();
    }
    pub(crate) fn router(&self) -> Router {
        router(self.state.clone())
    }
    fn new(
        run: &super::launch::WorkbenchRuntime,
        owner: &application::Owner,
        settings: super::config::ConfigHandle,
        shared: Arc<access::SharedAccess>,
    ) -> Result<Self> {
        let management = run
            .hub
            .history()
            .map(|history| {
                super::recording::management::Service::start(
                    history.root,
                    history.workspace,
                    run.epoch,
                    settings.clone(),
                )
            })
            .transpose()?;
        let (stop, stop_rx) = watch::channel(false);
        let state = Arc::new(WebState {
            hub: run.hub.clone(),
            authority: owner.authority.clone(),
            origin: owner.origin.clone(),
            pairing: owner.pairing.clone(),
            cookie_name: owner.cookie_name.clone(),
            session: owner.session.clone(),
            snapshot_slots: Arc::new(Semaphore::new(2)),
            stop: stop_rx,
            options: RuntimeOptions {
                workspace: Some(run.workspace.clone()),
                terminal: Some(run.terminal()),
                settings: Some(settings),
                management: management.as_ref().map(|m| m.handle()),
            },
            access: access::Access::new(),
            shared_access: Some(shared),
            terminal_slots: Arc::new(Semaphore::new(32)),
        });
        Ok(Self {
            state,
            stop,
            _management: management,
        })
    }
}
#[cfg(unix)]
impl Drop for RunSurface {
    fn drop(&mut self) {
        // Router clones and in-flight SSE/WS/device requests retain WebState.
        // Retiring this Run must end them even while Application stays alive.
        self.state.access.close();
        self.stop.send_replace(true);
    }
}
