//! Application lifecycle is independent of native CLI availability and run state.
use super::{
    config::{ConfigService, Overrides, Prepared},
    launch::{BrowserEntry, LaunchOptions, WorkbenchRuntime, entry::EntryFile},
    paths::WorkbenchPaths,
    web::application::{ApplicationServer, SurfaceHandle},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use tokio::sync::oneshot;
use uuid::Uuid;
pub(crate) mod folder_picker;
pub mod instance;
pub(crate) mod launching;
use launching::{
    Book, DirectoryGuard, LaunchResult, SharedBook, StartRequest, Target, TargetRequest,
};
#[cfg(test)]
pub(crate) mod sources;
pub use instance::{Acquisition, InstanceLock};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Connect {
    pub instance_id: Uuid,
    pub token: String,
    pub project: Option<PathBuf>,
    pub resume: Option<Uuid>,
    pub codex_bin: Option<String>,
    pub profile: Option<String>,
    pub provider_profile: Option<String>,
    pub native_home: Option<PathBuf>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub run_id: Uuid,
    pub project_name: String,
    pub project_path: PathBuf,
    pub native_home: PathBuf,
    pub cli_pid: u32,
    pub cli_version: String,
    pub resume: Option<Uuid>,
    pub native_thread_id: Option<Uuid>,
    pub state: String,
}
#[derive(Clone)]
pub(crate) struct RunHandle {
    sender: mpsc::SyncSender<Request>,
    book: SharedBook,
}
enum Request {
    Prepare(
        TargetRequest,
        oneshot::Sender<LaunchResult<serde_json::Value>>,
    ),
    Start(Uuid),
    Launch(
        Connect,
        oneshot::Sender<std::result::Result<RunSummary, &'static str>>,
    ),
    Wake,
    #[cfg(test)]
    Inspect(Box<dyn FnOnce(&mut Runner) + Send>),
}
impl RunHandle {
    pub async fn prepare(&self, request: TargetRequest) -> LaunchResult<serde_json::Value> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .try_send(Request::Prepare(request, tx))
            .map_err(|_| "launch_busy")?;
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| "launch_busy")?
            .map_err(|_| "launch_unavailable")?
    }
    pub fn begin(&self, request: StartRequest) -> LaunchResult<serde_json::Value> {
        self.book.lock().unwrap().begin(request, &self.sender)
    }
    pub fn operation(&self, id: Uuid) -> LaunchResult<serde_json::Value> {
        self.book.lock().unwrap().get(id)
    }

    pub async fn launch(&self, request: Connect) -> std::result::Result<RunSummary, &'static str> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .try_send(Request::Launch(request, tx))
            .map_err(|_| "launch_busy")?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .map_err(|_| "launch_unconfirmed")?
            .map_err(|_| "launch_unavailable")?
    }
}
struct RunWorker {
    handle: RunHandle,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
struct Runner {
    home: PathBuf,
    paths: WorkbenchPaths,
    data: PathBuf,
    config: super::config::ConfigHandle,
    surface: SurfaceHandle,
    library: crate::history::library::LibraryHandle,
    active: Vec<(WorkbenchRuntime, super::web::RunSurface, RunSummary)>,
}
// Bounded local concurrency, not a promise about all-machine performance. R6-G
// measures the combined resource budget before offering user tuning.
const MAX_RUNNING_RUNS: usize = 4;
const MAX_STOPPED_RUNS: usize = 4;
impl Runner {
    fn retire_stopped(&mut self) {
        while self
            .active
            .iter()
            .filter(|(run, _, _)| run.terminal().exited())
            .count()
            > MAX_STOPPED_RUNS
        {
            let index = self
                .active
                .iter()
                .position(|(run, _, _)| run.terminal().exited())
                .unwrap();
            self.surface.remove(self.active[index].2.run_id);
            self.active.remove(index);
        }
    }
    async fn launch(
        &mut self,
        cwd: PathBuf,
        home: PathBuf,
        resume: Option<Uuid>,
        target: Option<Arc<Target>>,
    ) -> LaunchResult<(RunSummary, bool)> {
        let directory = DirectoryGuard::open(&cwd)?;
        let cwd = directory.path.clone();
        let native_directory =
            DirectoryGuard::open(&home).map_err(|_| "native_home_unavailable")?;
        let home = native_directory.path.clone();
        let direct = if target.is_none() {
            resume
                .map(|id| crate::history::library::launch::NativeResume::direct(&home, id, &cwd))
                .transpose()?
        } else {
            None
        };
        let settings = self
            .config
            .read()
            .await
            .map_err(|_| "settings_unavailable")?;
        if !settings.errors.is_empty() {
            return Err("settings_invalid");
        }
        if let Some(target) = &target {
            target.verify(&self.config)?;
            if target.selection.project_id.is_some() || target.selection.resume_entry_id.is_some() {
                // Recheck source authorization and indexed identity at admission.
                let selection = self
                    .library
                    .launch_source(
                        target.selection.project_id.clone(),
                        target
                            .selection
                            .resume_entry_id
                            .clone()
                            .zip(target.selection.source_revision.clone()),
                    )
                    .await?;
                if selection
                    .path
                    .canonicalize()
                    .map_err(|_| "project_changed")?
                    != cwd
                {
                    return Err("project_changed");
                }
            }
        }
        for (run, _, summary) in &self.active {
            // Health failure does not prove process exit. Never start a second
            // CLI while this slot still owns a live terminal.
            if !run.terminal().exited() {
                if summary.project_path == cwd {
                    if !run.healthy() {
                        return Err("run_stopping");
                    }
                    if resume.is_none() {
                        return Ok((summary.clone(), true));
                    }
                    // Observed thread identity may be stale after /new or /fork;
                    // never use it to silently substitute the selected session.
                    return Err("project_session_running");
                }
                if resume.is_some()
                    && summary.native_home == home
                    && (summary.resume == resume || summary.native_thread_id == resume)
                {
                    // A conservative exclusion is safe even if an observed ID
                    // is stale. A live CLI never shares a resumed native thread.
                    return Err("native_session_running");
                }
            }
        }
        if self
            .active
            .iter()
            .filter(|(run, _, _)| !run.terminal().exited())
            .count()
            >= MAX_RUNNING_RUNS
        {
            return Err("run_capacity");
        }
        let launch = settings.effective.launch;
        let mut validation_error = None;
        let run = WorkbenchRuntime::start_checked(
            LaunchOptions {
                cwd,
                home: home.clone(),
                workbench_paths: self.paths.clone(),
                executable: launch.codex_bin.into(),
                native_profile: launch.profile,
                resume,
                entry_file: None,
                data_dir: Some(self.data.clone()),
            },
            || {
                let checked = directory
                    .verify()
                    .and_then(|_| native_directory.verify().map_err(|_| "native_home_changed"))
                    .and_then(|_| direct.as_ref().map_or(Ok(()), |native| native.verify()))
                    .and_then(|_| match &target {
                        Some(target) => target.verify(&self.config),
                        None => Ok(()),
                    });
                if let Err(code) = checked {
                    validation_error = Some(code);
                    anyhow::bail!(code);
                }
                Ok(())
            },
        )
        .await
        .map_err(|error| {
            let code = validation_error
                .unwrap_or_else(|| super::launch::failure::LaunchFailure::from_error(&error));
            eprintln!("codex-view: launch failed ({code}); native settings were not changed");
            code
        })?;
        let summary = RunSummary {
            run_id: run.epoch,
            project_name: run.terminal().project_name().into(),
            project_path: run.cwd.clone(),
            native_home: home,
            cli_pid: run.process_id(),
            cli_version: run.cli_version.clone(),
            resume,
            native_thread_id: resume,
            state: "running".into(),
        };
        let run_surface = self
            .surface
            .attach(&run, self.config.clone())
            .map_err(|_| "run_surface_failed")?;
        // Retire a previous stopped instance of this project only after the new
        // one is ready. Failed launch must leave all existing readers untouched.
        self.active.retain(|(old, _, previous)| {
            let keep = previous.project_path != summary.project_path || !old.terminal().exited();
            if !keep {
                self.surface.remove(previous.run_id);
            }
            keep
        });
        self.surface.publish(summary.clone(), run_surface.router());
        self.active.push((run, run_surface, summary.clone()));
        self.retire_stopped();
        Ok((summary, false))
    }
}
impl RunWorker {
    fn start(
        mut runner: Runner,
        sender: mpsc::SyncSender<Request>,
        receiver: mpsc::Receiver<Request>,
        book: SharedBook,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let records = book.clone();
        let thread = std::thread::Builder::new()
            .name("application-runs".into())
            .spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    match receiver.recv_timeout(Duration::from_millis(100)) {
                        Ok(Request::Prepare(request, reply)) if !reply.is_closed() => {
                            let result = runtime
                                .block_on(launching::prepare(
                                    request,
                                    &runner.home,
                                    &runner.paths,
                                    &runner.config,
                                    &runner.library,
                                ))
                                .and_then(|target| {
                                    let existing = runner
                                        .active
                                        .iter()
                                        .find(|(_, _, summary)| {
                                            summary.project_path == target.directory.path
                                        })
                                        .map(|(_, _, summary)| summary.clone());
                                    let open_in_new_tab =
                                        runner.active.iter().any(|(run, _, summary)| {
                                            !run.terminal().exited()
                                                && summary.project_path != target.directory.path
                                        });
                                    records
                                        .lock()
                                        .unwrap()
                                        .add(target, existing, open_in_new_tab)
                                });
                            let _ = reply.send(result);
                        }
                        Ok(Request::Start(id)) => {
                            let target = records.lock().unwrap().target(id);
                            // Once admitted, losing the HTTP reply never cancels or repeats spawn.
                            let result = runtime.block_on(runner.launch(
                                target.directory.path.clone(),
                                target.native_home.clone(),
                                target.native.as_ref().map(|n| n.id),
                                Some(target),
                            ));
                            runner.surface.launch_result(result.as_ref().err().copied());
                            let open_in_new_tab = result.as_ref().is_ok_and(|(selected, _)| {
                                runner.active.iter().any(|(run, _, summary)| {
                                    !run.terminal().exited() && summary.run_id != selected.run_id
                                })
                            });
                            records.lock().unwrap().finish(id, result, open_in_new_tab);
                        }
                        Ok(Request::Launch(request, reply)) if !reply.is_closed() => {
                            let result = match request.project {
                                Some(cwd) => runtime
                                    .block_on(runner.launch(
                                        cwd,
                                        runner.home.clone(),
                                        request.resume,
                                        None,
                                    ))
                                    .map(|(summary, _)| summary),
                                None => Err("project_required"),
                            };
                            runner.surface.launch_result(result.as_ref().err().copied());
                            let _ = reply.send(result);
                        }
                        #[cfg(test)]
                        Ok(Request::Inspect(inspect)) => inspect(&mut runner),
                        _ => {}
                    }
                    for (run, surface, summary) in &mut runner.active {
                        let native = run
                            .hub
                            .user_targets()
                            .last()
                            .map(|(id, _)| *id)
                            .or(summary.resume);
                        if summary.native_thread_id != native {
                            summary.native_thread_id = native;
                            runner.surface.summary(summary.clone());
                        }
                        if !run.healthy() && !run.terminal().exited() {
                            // A broken proxy cannot leave an orphan interactive CLI.
                            let _ = runtime.block_on(run.terminal().stop());
                        }
                        if run.terminal().exited() && summary.state == "running" {
                            surface.disable_devices();
                            summary.state = "stopped".into();
                            runner.surface.summary(summary.clone());
                        }
                    }
                    runner.retire_stopped();
                }
                runner.surface.clear();
                drop(runner.active);
            })?;
        Ok(Self {
            handle: RunHandle { sender, book },
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for RunWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.handle.sender.try_send(Request::Wake);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub struct Application {
    // All child resources stop before application credentials and lock disappear.
    _runs: RunWorker,
    pub web: ApplicationServer,
    _sources: crate::history::library::HistoryLibrary,
    _settings: ConfigService,
    _entry: EntryFile,
    _custom_entry: Option<EntryFile>,
    lock: InstanceLock,
    pub instance_id: Uuid,
    pub entry: BrowserEntry,
}
impl Application {
    pub fn start(
        prepared: Prepared,
        home: PathBuf,
        paths: WorkbenchPaths,
        lock: InstanceLock,
        custom: Option<&Path>,
    ) -> Result<Self> {
        let data = prepared.data_dir.clone();
        let settings = ConfigService::start(prepared)?;
        let id = Uuid::new_v4();
        let (sender, receiver) = mpsc::sync_channel(4);
        let book = Arc::new(std::sync::Mutex::new(Book::default()));
        let sources = crate::history::library::HistoryLibrary::start(
            home.clone(),
            data.clone(),
            settings.handle(),
        )?;
        let web = ApplicationServer::bind(
            id,
            settings.handle(),
            RunHandle {
                sender: sender.clone(),
                book: book.clone(),
            },
            sources.handle(),
            home.clone(),
        )?;
        let runs = RunWorker::start(
            Runner {
                home,
                paths,
                data,
                config: settings.handle(),
                surface: web.handle(),
                library: sources.handle(),
                active: Vec::new(),
            },
            sender,
            receiver,
            book,
        )?;
        let entry = BrowserEntry {
            format: "codex-view-entry-v2".into(),
            instance_id: Some(id),
            address: web.origin(),
            url: web.bootstrap_url(None),
            ..Default::default()
        };
        lock.verify()?;
        let entry_file = EntryFile::create(Some(&lock.path()), &entry)?;
        let custom_entry = custom
            .map(|path| EntryFile::create(Some(path), &entry))
            .transpose()?;
        Ok(Self {
            _runs: runs,
            web,
            _sources: sources,
            _settings: settings,
            _entry: entry_file,
            _custom_entry: custom_entry,
            lock,
            instance_id: id,
            entry,
        })
    }
    pub fn entry_file(&self) -> &Path {
        self._custom_entry
            .as_ref()
            .unwrap_or(&self._entry)
            .path
            .as_path()
    }
    pub fn healthy(&self) -> bool {
        self.web.healthy()
            && self.lock.verify().is_ok()
            && self._runs.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

/// Verify the private entry by an authenticated, non-redirecting loopback handshake.
pub async fn connect(entry: &BrowserEntry, mut request: Connect) -> Result<Option<RunSummary>> {
    request.instance_id = entry
        .instance_id
        .ok_or_else(|| anyhow::anyhow!("application entry v2 required"))?;
    request.token = reqwest::Url::parse(&entry.url)?
        .fragment()
        .and_then(|s| s.strip_prefix("pair="))
        .ok_or_else(|| anyhow::anyhow!("invalid private entry"))?
        .to_owned();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()?;
    let response = client
        .post(format!(
            "{}/workbench/v1/application/connect",
            entry.address
        ))
        .header("Origin", &entry.address)
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&request)?)
        .send()
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "application handshake unavailable; retry without removing the instance lock"
            )
        })?;
    let status = response.status();
    let bytes = response.bytes().await?;
    ensure!(
        bytes.len() < 32 * 1024,
        "application handshake exceeds bound"
    );
    let body: serde_json::Value = serde_json::from_slice(&bytes)?;
    if !status.is_success() {
        let code = body
            .pointer("/error/code")
            .and_then(|v| v.as_str())
            .unwrap_or("handshake_failed");
        let safe = [
            "launch_defaults_conflict",
            "native_home_conflict",
            "project_already_running",
            "project_session_running",
            "native_session_running",
            "run_capacity",
            "run_stopping",
            "native_launch_failed",
            "project_unavailable",
            "settings_invalid",
            "launch_unconfirmed",
            "launch_busy",
            "invalid_instance",
            "pairing_required",
        ];
        anyhow::bail!(
            "application request failed: {}",
            if safe.contains(&code)
                || super::launch::failure::LaunchFailure::ALL
                    .iter()
                    .any(|e| e.code() == code)
            {
                code
            } else {
                "handshake_failed"
            }
        );
    }
    ensure!(
        body["instanceId"] == request.instance_id.to_string(),
        "application identity mismatch"
    );
    Ok(serde_json::from_value(body["run"].clone())?)
}

pub fn request(
    overrides: &Overrides,
    project: Option<PathBuf>,
    resume: Option<Uuid>,
    native_home: PathBuf,
) -> Connect {
    Connect {
        project,
        resume,
        codex_bin: overrides
            .codex_bin
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        profile: overrides.profile.clone(),
        provider_profile: overrides.provider_profile.clone(),
        native_home: Some(native_home),
        ..Default::default()
    }
}
#[cfg(test)]
mod tests;
