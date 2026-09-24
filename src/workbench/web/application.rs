//! One local owner surface; the per-Run device router never includes these APIs.
use super::*;
use crate::workbench::application::launching::{StartRequest, TargetRequest};
use crate::workbench::{
    application::{Connect, RunHandle, RunSummary},
    config::{Config, ConfigError, ConfigHandle, LIMIT},
};
use std::{path::PathBuf, sync::RwLock};
use tower::ServiceExt;

pub(crate) struct Owner {
    pub authority: String,
    pub origin: String,
    pub pairing: String,
    pub cookie_name: String,
    pub session: String,
}
struct StateData {
    id: Uuid,
    owner: Owner,
    settings: ConfigHandle,
    runs: RunHandle,
    sources: crate::history::library::LibraryHandle,
    native_home: PathBuf,
    run: RwLock<Vec<(RunSummary, Router)>>,
    devices: Arc<access::SharedAccess>,
    launch_error: RwLock<Option<&'static str>>,
    picker: crate::workbench::application::folder_picker::FolderPicker,
}
#[derive(Clone)]
pub(crate) struct SurfaceHandle(Arc<StateData>);
impl SurfaceHandle {
    pub fn attach(
        &self,
        run: &crate::workbench::launch::WorkbenchRuntime,
        config: ConfigHandle,
    ) -> Result<RunSurface> {
        RunSurface::new(run, &self.0.owner, config, self.0.devices.clone())
    }
    pub fn publish(&self, summary: RunSummary, router: Router) {
        self.0.run.write().unwrap().push((summary, router));
    }
    pub fn summary(&self, summary: RunSummary) {
        if let Some(run) = self
            .0
            .run
            .write()
            .unwrap()
            .iter_mut()
            .find(|r| r.0.run_id == summary.run_id)
        {
            run.0 = summary;
        }
    }
    pub fn launch_result(&self, code: Option<&'static str>) {
        *self.0.launch_error.write().unwrap() = code;
    }
    pub fn clear(&self) {
        self.0.run.write().unwrap().clear();
    }
    pub fn remove(&self, id: Uuid) {
        self.0.run.write().unwrap().retain(|r| r.0.run_id != id);
    }
}
pub struct ApplicationServer {
    state: Arc<StateData>,
    stop: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}
impl ApplicationServer {
    pub(crate) fn bind(
        id: Uuid,
        settings: ConfigHandle,
        runs: RunHandle,
        sources: crate::history::library::LibraryHandle,
        native_home: PathBuf,
    ) -> Result<Self> {
        anyhow::ensure!(
            WorkbenchAssets::get("workbench.html").is_some(),
            "build workbench frontend first"
        );
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let (stop, stop_rx) = watch::channel(false);
        let state = Arc::new(StateData {
            id,
            owner: Owner {
                authority: address.to_string(),
                origin: format!("http://{address}"),
                pairing: secret(),
                cookie_name: format!("{COOKIE}_{}", id.simple()),
                session: secret(),
            },
            settings,
            runs,
            sources,
            native_home,
            run: RwLock::new(Vec::new()),
            devices: access::SharedAccess::new(),
            launch_error: RwLock::new(None),
            picker: crate::workbench::application::folder_picker::FolderPicker::new(),
        });
        let app = Router::new()
            .route("/", get(home))
            .route("/assets/{*path}", get(asset))
            .route("/workbench/v1/pair", post(pair_owner))
            .route("/workbench/v1/application", get(application))
            .route("/workbench/v1/application/connect", post(connect))
            .route("/workbench/v1/launch-targets", post(launch_target))
            .route(
                "/workbench/v1/application/pick-directory",
                post(pick_directory),
            )
            .route("/workbench/v1/runs", get(list_runs).post(start_run))
            .route("/workbench/v1/runs/{id}", get(run_summary))
            .route(
                "/workbench/v1/launch-operations/{id}",
                get(launch_operation),
            )
            .route(
                "/workbench/v1/application/settings",
                get(settings_read).put(settings_save),
            )
            .route("/workbench/v1/library/sources", get(source_status))
            .route("/workbench/v1/library/projects", get(library_projects))
            .route("/workbench/v1/library/entries", get(library_entries))
            .route("/workbench/v1/library/entries/{entry}", get(library_body))
            .route(
                "/workbench/v1/library/entries/{entry}/details",
                get(library_details),
            )
            .route("/workbench/v1/library/refresh", post(library_refresh))
            .route("/workbench/v1/library/preview", post(library_preview))
            .fallback(dispatch)
            .layer(DefaultBodyLimit::max(LIMIT))
            .layer(middleware::from_fn_with_state(state.clone(), boundary))
            .with_state(state.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let thread = std::thread::Builder::new().name("application-web".into()).spawn(move || runtime.block_on(async move {
            let mut stopped = stop_rx;
            tokio::select! { biased; _ = stopped.changed() => {}, _ = axum::serve(TcpListener::from_std(listener).expect("application listener"), app) => {} }
        }))?;
        Ok(Self {
            state,
            stop,
            thread: Some(thread),
        })
    }
    #[cfg(test)]
    pub(crate) fn set_picker_program_for_test(&self, program: PathBuf) {
        *self.state.picker.test_program.lock().unwrap() = Some(program);
    }
    pub(crate) fn handle(&self) -> SurfaceHandle {
        SurfaceHandle(self.state.clone())
    }
    pub fn origin(&self) -> String {
        self.state.owner.origin.clone()
    }
    pub fn bootstrap_url(&self, run: Option<Uuid>) -> String {
        format!(
            "{}/{}#pair={}",
            self.state.owner.origin,
            run.map(|id| format!("?run={id}")).unwrap_or_default(),
            self.state.owner.pairing
        )
    }
    pub fn healthy(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}
impl Drop for ApplicationServer {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn authorised(headers: &HeaderMap, state: &StateData) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|v| v.trim().split_once('='))
        .any(|(key, value)| {
            key == state.owner.cookie_name && matches_secret(value, &state.owner.session)
        })
}
fn origin(headers: &HeaderMap, state: &StateData) -> bool {
    headers.get_all(header::ORIGIN).iter().count() == 1
        && headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some(&state.owner.origin)
}
async fn boundary(
    State(state): State<Arc<StateData>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let valid = headers.get_all(header::HOST).iter().count() == 1
        && headers.get(header::HOST).and_then(|v| v.to_str().ok()) == Some(&state.owner.authority)
        && (!headers.contains_key(header::ORIGIN) || origin(headers, &state))
        && headers
            .get("sec-fetch-site")
            .is_none_or(|v| matches!(v.to_str(), Ok("same-origin" | "none")))
        && request.uri().scheme().is_none();
    let mut response = if valid {
        next.run(request).await
    } else {
        error(StatusCode::FORBIDDEN, "invalid_origin_or_host")
    };
    for (name, value) in [
        (header::CACHE_CONTROL, "no-store"),
        (header::CONTENT_SECURITY_POLICY, WORKBENCH_CSP),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}
async fn home() -> Response {
    match WorkbenchAssets::get("workbench.html") {
        Some(file) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            file.data.into_owned(),
        )
            .into_response(),
        None => error(StatusCode::SERVICE_UNAVAILABLE, "frontend_unavailable"),
    }
}
async fn pair_owner(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !origin(&headers, &state) {
        return error(StatusCode::FORBIDDEN, "origin_required");
    }
    if body.len() > 1024 {
        return error(StatusCode::BAD_REQUEST, "invalid_pair");
    }
    let Ok(pair) = serde_json::from_slice::<Pair>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_pair");
    };
    if !matches_secret(&pair.token, &state.owner.pairing) {
        return error(StatusCode::FORBIDDEN, "invalid_pair");
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{}={}; HttpOnly; SameSite=Strict; Path=/",
            state.owner.cookie_name, state.owner.session
        ))
        .unwrap(),
    );
    response
}
async fn application(State(state): State<Arc<StateData>>, headers: HeaderMap) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    let runs: Vec<_> = state
        .run
        .read()
        .unwrap()
        .iter()
        .map(|r| r.0.clone())
        .collect();
    Json(serde_json::json!({"instanceId":state.id,"mode":"history-home","settingsAvailable":true,"libraryAvailable":true,"launchAvailable":true,"directoryPickerAvailable":crate::workbench::application::folder_picker::FolderPicker::supported(),"runs":runs,"sources":state.sources.statuses(),"launchError":*state.launch_error.read().unwrap()})).into_response()
}
fn launch_result(
    result: std::result::Result<serde_json::Value, &'static str>,
    accepted: bool,
) -> Response {
    match result {
        Ok(value) => (
            if accepted {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            },
            Json(value),
        )
            .into_response(),
        Err(code) => error(
            match code {
                "operation_unavailable" | "entry_unavailable" => StatusCode::NOT_FOUND,
                "launch_busy" | "run_capacity" | "launch_unavailable" | "library_busy"
                | "library_timeout" => StatusCode::SERVICE_UNAVAILABLE,
                "operation_conflict"
                | "target_expired"
                | "config_changed"
                | "project_changed"
                | "source_revision_changed"
                | "source_revoked"
                | "invalid_instance" => StatusCode::CONFLICT,
                _ => StatusCode::UNPROCESSABLE_ENTITY,
            },
            code,
        ),
    }
}
fn launch_owner(
    headers: &HeaderMap,
    state: &StateData,
) -> std::result::Result<(), (StatusCode, &'static str)> {
    if !authorised(headers, state) {
        return Err((StatusCode::UNAUTHORIZED, "pairing_required"));
    }
    if !origin(headers, state) {
        return Err((StatusCode::FORBIDDEN, "origin_required"));
    }
    if headers.get_all(header::CONTENT_TYPE).iter().count() != 1
        || headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            != Some("application/json")
    {
        return Err((StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required"));
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PickDirectory {
    instance_id: Uuid,
}
async fn pick_directory(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err((status, code)) = launch_owner(&headers, &state) {
        return error(status, code);
    }
    let Ok(request) = serde_json::from_slice::<PickDirectory>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_picker_request");
    };
    if request.instance_id != state.id {
        return error(StatusCode::CONFLICT, "invalid_instance");
    }
    match state.picker.select().await {
        Ok(path) => Json(serde_json::json!({"path":path})).into_response(),
        Err(code) => error(
            if code == "picker_busy" {
                StatusCode::CONFLICT
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            },
            code,
        ),
    }
}

async fn launch_target(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err((status, code)) = launch_owner(&headers, &state) {
        return error(status, code);
    }
    let Ok(request) = serde_json::from_slice::<TargetRequest>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_launch_target");
    };
    launch_result(state.runs.prepare(request).await, false)
}
async fn start_run(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err((status, code)) = launch_owner(&headers, &state) {
        return error(status, code);
    }
    let Ok(request) = serde_json::from_slice::<StartRequest>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_launch_request");
    };
    if request.instance_id != state.id {
        return error(StatusCode::CONFLICT, "invalid_instance");
    }
    launch_result(state.runs.begin(request), true)
}
async fn launch_operation(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    launch_result(state.runs.operation(id), false)
}
async fn list_runs(State(state): State<Arc<StateData>>, headers: HeaderMap) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    Json(serde_json::json!({"runs":state.run.read().unwrap().iter().map(|r|r.0.clone()).collect::<Vec<_>>()})).into_response()
}
async fn run_summary(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    match state.run.read().unwrap().iter().find(|r| r.0.run_id == id) {
        Some(run) => Json(serde_json::json!(run.0)).into_response(),
        None => error(StatusCode::NOT_FOUND, "run_unavailable"),
    }
}
async fn source_status(State(state): State<Arc<StateData>>, headers: HeaderMap) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    library_result(state.sources.sources().await)
}
async fn connect(State(state): State<Arc<StateData>>, headers: HeaderMap, body: Bytes) -> Response {
    if !origin(&headers, &state) {
        return error(StatusCode::FORBIDDEN, "origin_required");
    }
    let Ok(request) = serde_json::from_slice::<Connect>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_connect");
    };
    if request.instance_id != state.id {
        return error(StatusCode::CONFLICT, "invalid_instance");
    }
    if !matches_secret(&request.token, &state.owner.pairing) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    let settings = match state.settings.read().await {
        Ok(s) => s,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "settings_unavailable"),
    };
    let launch = settings.effective.launch;
    if request
        .codex_bin
        .as_ref()
        .is_some_and(|v| v != &launch.codex_bin)
        || request
            .profile
            .as_ref()
            .is_some_and(|v| Some(v) != launch.profile.as_ref())
        || request
            .provider_profile
            .as_ref()
            .is_some_and(|v| v != &launch.provider_profile)
    {
        return error(StatusCode::CONFLICT, "launch_defaults_conflict");
    }
    if request
        .native_home
        .as_ref()
        .is_some_and(|v| v != &state.native_home)
    {
        return error(StatusCode::CONFLICT, "native_home_conflict");
    }
    let run = if request.project.is_some() {
        match state.runs.launch(request).await {
            Ok(run) => Some(run),
            Err(code) => return error(StatusCode::CONFLICT, code),
        }
    } else if request.resume.is_some() {
        return error(StatusCode::BAD_REQUEST, "project_required");
    } else {
        None
    };
    Json(serde_json::json!({"instanceId":state.id,"run":run})).into_response()
}
async fn dispatch(
    State(state): State<Arc<StateData>>,
    mut request: axum::extract::Request,
) -> Response {
    let path = request.uri().path();
    let router = if let Some(rest) = path.strip_prefix("/workbench/v1/runs/") {
        let Some((id, tail)) = rest.split_once('/') else {
            return error(StatusCode::NOT_FOUND, "run_unavailable");
        };
        // Direct owner bootstrap URLs contain a Run ID before the owner Cookie
        // exists. Only the scoped pairing handler is public; it validates token
        // and same-origin independently.
        if tail != "pair" && !authorised(request.headers(), &state) {
            return error(StatusCode::UNAUTHORIZED, "pairing_required");
        }
        let Ok(id) = Uuid::parse_str(id) else {
            return error(StatusCode::NOT_FOUND, "run_unavailable");
        };
        let router = state
            .run
            .read()
            .unwrap()
            .iter()
            .find(|r| r.0.run_id == id)
            .map(|r| r.1.clone());
        let tail = if tail == "stop" { "run/stop" } else { tail };
        let uri = format!(
            "/workbench/v1/{tail}{}",
            request
                .uri()
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default()
        );
        let Ok(uri) = uri.parse() else {
            return error(StatusCode::BAD_REQUEST, "invalid_run_path");
        };
        *request.uri_mut() = uri;
        router
    } else {
        // Application clients must name a Run, including when only one exists.
        // Never select a project implicitly from mutable global state.
        None
    };
    match router {
        Some(router) => router.oneshot(request).await.unwrap(),
        None => error(StatusCode::NOT_FOUND, "run_unavailable"),
    }
}
fn settings_result(
    state: &StateData,
    value: std::result::Result<crate::workbench::config::Settings, ConfigError>,
) -> Response {
    match value {
        Ok(settings) => {
            Json(serde_json::json!({"instanceId":state.id,"settings":settings})).into_response()
        }
        Err(error) => (
            if error.code == "config_changed" {
                StatusCode::PRECONDITION_FAILED
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            },
            Json(serde_json::json!({"error":error})),
        )
            .into_response(),
    }
}
async fn settings_read(State(state): State<Arc<StateData>>, headers: HeaderMap) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    settings_result(&state, state.settings.read().await)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Save {
    instance_id: Uuid,
    config: Box<serde_json::value::RawValue>,
}
async fn settings_save(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    if !origin(&headers, &state) {
        return error(StatusCode::FORBIDDEN, "origin_required");
    }
    if headers.get_all(header::CONTENT_TYPE).iter().count() != 1
        || headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(';').next())
            != Some("application/json")
    {
        return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required");
    }
    let Ok(request) = serde_json::from_slice::<Save>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_config_request");
    };
    if request.instance_id != state.id {
        return error(StatusCode::CONFLICT, "stale_instance");
    }
    let Some(revision) = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix('"'))
        .and_then(|s| s.strip_suffix('"'))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return error(
            StatusCode::PRECONDITION_REQUIRED,
            "config_revision_required",
        );
    };
    if headers.get_all(header::IF_MATCH).iter().count() != 1 {
        return error(StatusCode::BAD_REQUEST, "invalid_config_revision");
    }
    match Config::parse(request.config.get().as_bytes()) {
        Ok(config) => settings_result(&state, state.settings.save(revision.into(), config).await),
        Err(e) => settings_result(&state, Err(e)),
    }
}

fn library_result(result: std::result::Result<serde_json::Value, &'static str>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(code) => error(
            match code {
                "stale_cursor"
                | "source_revoked"
                | "source_revision_changed"
                | "search_position_unavailable" => StatusCode::CONFLICT,
                "entry_unavailable" | "details_unavailable" => StatusCode::NOT_FOUND,
                "invalid_cursor" | "invalid_query" => StatusCode::BAD_REQUEST,
                _ => StatusCode::SERVICE_UNAVAILABLE,
            },
            code,
        ),
    }
}
async fn library_projects(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    Query(query): Query<crate::history::library::Query>,
) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    library_result(state.sources.list(query, true).await)
}
async fn library_entries(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    Query(query): Query<crate::history::library::Query>,
) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    library_result(state.sources.list(query, false).await)
}
async fn library_body(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    Path(entry): Path<String>,
    Query(query): Query<crate::history::library::Query>,
) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    library_result(state.sources.body(entry, query).await)
}
async fn library_refresh(State(state): State<Arc<StateData>>, headers: HeaderMap) -> Response {
    if !authorised(&headers, &state) || !origin(&headers, &state) {
        return error(StatusCode::FORBIDDEN, "owner_required");
    }
    match state.sources.refresh().await {
        Ok(value) => (StatusCode::ACCEPTED, Json(value)).into_response(),
        Err(code) => error(StatusCode::SERVICE_UNAVAILABLE, code),
    }
}
async fn library_preview(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorised(&headers, &state) || !origin(&headers, &state) {
        return error(StatusCode::FORBIDDEN, "owner_required");
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Preview {
        path: PathBuf,
    }
    let Ok(request) = serde_json::from_slice::<Preview>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_preview");
    };
    library_result(state.sources.preview(request.path).await)
}

async fn library_details(
    State(state): State<Arc<StateData>>,
    headers: HeaderMap,
    Path(entry): Path<String>,
    Query(query): Query<crate::history::library::Query>,
) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    library_result(state.sources.details(entry, query).await)
}
