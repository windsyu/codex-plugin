//! Short-lived launch targets and in-memory operation results, never an input ledger.
use super::*;
use crate::history::library::launch::NativeResume;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::sync::Mutex;
use std::time::Instant;

pub type LaunchResult<T> = std::result::Result<T, &'static str>;
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetRequest {
    pub project_id: Option<String>,
    pub path: Option<String>,
    pub resume_entry_id: Option<String>,
    pub source_revision: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartRequest {
    pub instance_id: Uuid,
    pub target_id: Uuid,
    pub mode: Mode,
    pub operation_id: Uuid,
    pub config_revision: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    New,
    Resume,
}

pub(super) struct DirectoryGuard {
    pub path: PathBuf,
    file: File,
}
impl DirectoryGuard {
    pub fn open(path: &Path) -> LaunchResult<Self> {
        let path = path.canonicalize().map_err(|_| "project_unavailable")?;
        if path.ancestors().count() > 64 {
            return Err("invalid_project_path");
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|_| "project_unavailable")?;
        let meta = file.metadata().map_err(|_| "project_unavailable")?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
            return Err("project_unavailable");
        }
        let guard = Self { path, file };
        guard.verify()?;
        Ok(guard)
    }
    pub fn verify(&self) -> LaunchResult<()> {
        let now = std::fs::symlink_metadata(&self.path).map_err(|_| "project_changed")?;
        let original = self.file.metadata().map_err(|_| "project_changed")?;
        if !now.is_dir()
            || now.dev() != original.dev()
            || now.ino() != original.ino()
            || now.uid() != original.uid()
        {
            return Err("project_changed");
        }
        Ok(())
    }
}
pub(super) struct Target {
    pub id: Uuid,
    pub directory: DirectoryGuard,
    pub native_home: PathBuf,
    pub native_directory: DirectoryGuard,
    pub native: Option<NativeResume>,
    pub revision: String,
    pub selection: TargetRequest,
    created: Instant,
}
impl Target {
    fn preview(&self, existing: Option<RunSummary>, open_in_new_tab: bool) -> Value {
        json!({"targetId":self.id,"canonicalPath":self.directory.path,"configRevision":self.revision,"nativeHome":self.native_home,"modes":if self.native.is_some(){vec!["resume"]}else{vec!["new"]},"expiresInSeconds":60,"existingRun":existing,"openInNewTab":open_in_new_tab})
    }
    pub fn verify(&self, config: &super::super::config::ConfigHandle) -> LaunchResult<()> {
        self.directory.verify()?;
        self.native_directory
            .verify()
            .map_err(|_| "native_home_changed")?;
        let (_, revision) = config.history_policy().map_err(|_| "settings_invalid")?;
        if revision != self.revision {
            return Err("config_changed");
        }
        if let Some(native) = &self.native {
            native.verify()?;
        }
        Ok(())
    }
}
struct OperationState {
    request: StartRequest,
    target: Arc<Target>,
    status: &'static str,
    run: Option<RunSummary>,
    error: Option<&'static str>,
    open_in_new_tab: bool,
    touched: Instant,
}
impl OperationState {
    fn value(&self) -> Value {
        json!({"operationId":self.request.operation_id,"state":self.status,"run":self.run,"url":self.run.as_ref().map(|run|format!("/?run={}",run.run_id)),"error":self.error.map(|code|json!({"code":code})),"openInNewTab":self.open_in_new_tab})
    }
}
#[derive(Default)]
pub(super) struct Book {
    targets: HashMap<Uuid, Arc<Target>>,
    operations: HashMap<Uuid, OperationState>,
}
pub(super) type SharedBook = Arc<Mutex<Book>>;
impl Book {
    fn prune(&mut self) {
        self.targets
            .retain(|_, t| t.created.elapsed() < Duration::from_secs(60));
        self.operations.retain(|_, o| {
            o.status == "starting" || o.touched.elapsed() < Duration::from_secs(900)
        });
    }
    pub fn add(
        &mut self,
        target: Target,
        existing: Option<RunSummary>,
        open_in_new_tab: bool,
    ) -> LaunchResult<Value> {
        self.prune();
        if self.targets.len() >= 64 {
            return Err("launch_busy");
        }
        let value = target.preview(existing, open_in_new_tab);
        self.targets.insert(target.id, Arc::new(target));
        Ok(value)
    }
    pub fn begin(
        &mut self,
        request: StartRequest,
        sender: &mpsc::SyncSender<Request>,
    ) -> LaunchResult<Value> {
        self.prune();
        if let Some(old) = self.operations.get(&request.operation_id) {
            return if old.request == request {
                Ok(old.value())
            } else {
                Err("operation_conflict")
            };
        }
        if request.operation_id.is_nil() || request.target_id.is_nil() {
            return Err("invalid_launch_request");
        }
        if self.operations.len() >= 256 || self.operations.values().any(|o| o.status == "starting")
        {
            return Err("launch_busy");
        }
        let target = self
            .targets
            .get(&request.target_id)
            .ok_or("target_expired")?
            .clone();
        if request.config_revision != target.revision {
            return Err("config_changed");
        }
        if (request.mode == Mode::Resume) != target.native.is_some() {
            return Err("invalid_launch_mode");
        }
        let id = request.operation_id;
        self.operations.insert(
            id,
            OperationState {
                request,
                target,
                status: "starting",
                run: None,
                error: None,
                open_in_new_tab: false,
                touched: Instant::now(),
            },
        );
        if sender.try_send(Request::Start(id)).is_err() {
            self.operations.remove(&id);
            return Err("launch_busy");
        }
        Ok(self.operations[&id].value())
    }
    pub fn get(&self, id: Uuid) -> LaunchResult<Value> {
        self.operations
            .get(&id)
            .filter(|o| o.status == "starting" || o.touched.elapsed() < Duration::from_secs(900))
            .map(OperationState::value)
            .ok_or("operation_unavailable")
    }
    pub fn target(&self, id: Uuid) -> Arc<Target> {
        self.operations[&id].target.clone()
    }
    pub fn finish(
        &mut self,
        id: Uuid,
        result: LaunchResult<(RunSummary, bool)>,
        open_in_new_tab: bool,
    ) {
        if let Some(operation) = self.operations.get_mut(&id) {
            operation.touched = Instant::now();
            match result {
                Ok((run, existing)) => {
                    operation.status = if existing { "existing" } else { "ready" };
                    operation.run = Some(run);
                    operation.open_in_new_tab = open_in_new_tab;
                }
                Err(code) => {
                    operation.status = "failed";
                    operation.error = Some(code);
                }
            }
        }
    }
}

pub(super) async fn prepare(
    request: TargetRequest,
    home: &Path,
    paths: &WorkbenchPaths,
    config: &super::super::config::ConfigHandle,
    library: &crate::history::library::LibraryHandle,
) -> LaunchResult<Target> {
    if (request.path.is_some() && request.project_id.is_some())
        || (request.resume_entry_id.is_none()
            && request.path.is_none()
            && request.project_id.is_none())
        || request.resume_entry_id.is_some() != request.source_revision.is_some()
        || request.project_id.as_ref().is_some_and(|id| id.len() > 128)
        || request
            .resume_entry_id
            .as_ref()
            .is_some_and(|id| id.len() > 128)
        || request
            .source_revision
            .as_ref()
            .is_some_and(|id| id.len() != 64)
    {
        return Err("invalid_launch_target");
    }
    let settings = config.read().await.map_err(|_| "settings_unavailable")?;
    if !settings.errors.is_empty() {
        return Err("settings_invalid");
    }
    let (path, native) = if let Some(id) = &request.resume_entry_id {
        let selected = library
            .launch_source(
                request.project_id.clone(),
                Some((id.clone(), request.source_revision.clone().unwrap())),
            )
            .await?;
        if let Some(path) = &request.path
            && expand(path, paths)?
                .canonicalize()
                .map_err(|_| "project_unavailable")?
                != selected.path
        {
            return Err("project_changed");
        }
        (selected.path, selected.resume)
    } else if let Some(project) = &request.project_id {
        (
            library
                .launch_source(Some(project.clone()), None)
                .await?
                .path,
            None,
        )
    } else {
        (expand(request.path.as_ref().unwrap(), paths)?, None)
    };
    let directory = DirectoryGuard::open(&path)?;
    let native_home = native.as_ref().map_or(home, |n| n.home.as_path());
    let native_directory =
        DirectoryGuard::open(native_home).map_err(|_| "native_home_unavailable")?;
    Ok(Target {
        id: Uuid::new_v4(),
        directory,
        native_home: native_directory.path.clone(),
        native_directory,
        native,
        revision: settings.revision.ok_or("settings_invalid")?,
        selection: request,
        created: Instant::now(),
    })
}
fn expand(value: &str, paths: &WorkbenchPaths) -> LaunchResult<PathBuf> {
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err("invalid_project_path");
    }
    let path = if value == "~" {
        paths.root.parent().unwrap().to_path_buf()
    } else if let Some(tail) = value.strip_prefix("~/") {
        paths.root.parent().unwrap().join(tail)
    } else {
        PathBuf::from(value)
    };
    if !path.is_absolute() {
        return Err("absolute_path_required");
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(root: &Path) -> Target {
        Target {
            id: Uuid::new_v4(),
            directory: DirectoryGuard::open(root).unwrap(),
            native_home: root.into(),
            native_directory: DirectoryGuard::open(root).unwrap(),
            native: None,
            revision: "a".repeat(64),
            selection: TargetRequest {
                path: Some(root.to_string_lossy().into_owned()),
                ..Default::default()
            },
            created: Instant::now(),
        }
    }
    fn request(target: &Target) -> StartRequest {
        StartRequest {
            instance_id: Uuid::new_v4(),
            target_id: target.id,
            mode: Mode::New,
            operation_id: Uuid::new_v4(),
            config_revision: target.revision.clone(),
        }
    }
    #[test]
    fn targets_expire_but_accepted_operation_is_queryable_and_never_queued_twice() {
        let tmp = tempfile::tempdir().unwrap();
        let mut book = Book::default();
        let t = target(tmp.path());
        let req = request(&t);
        book.add(t, None, false).unwrap();
        let (tx, rx) = mpsc::sync_channel(4);
        book.begin(req.clone(), &tx).unwrap();
        assert!(matches!(rx.recv().unwrap(), Request::Start(_)));
        book.targets.clear();
        assert_eq!(book.begin(req.clone(), &tx).unwrap()["state"], "starting");
        assert!(rx.try_recv().is_err());
        assert_eq!(book.get(req.operation_id).unwrap()["state"], "starting");
        let mut other = req.clone();
        other.config_revision = "b".repeat(64);
        assert_eq!(book.begin(other, &tx).unwrap_err(), "operation_conflict");
        book.finish(req.operation_id, Err("native_launch_failed"), false);
        assert_eq!(book.begin(req.clone(), &tx).unwrap()["state"], "failed");
        assert!(rx.try_recv().is_err());
        let mut expired = target(tmp.path());
        expired.created = Instant::now() - Duration::from_secs(61);
        let req = request(&expired);
        book.add(expired, None, false).unwrap();
        assert_eq!(book.begin(req, &tx).unwrap_err(), "target_expired");
    }
    #[test]
    fn launch_book_rejects_mode_conflicts_capacity_and_cleans_failed_admission() {
        let tmp = tempfile::tempdir().unwrap();
        let mut book = Book::default();
        let t = target(tmp.path());
        let mut req = request(&t);
        book.add(t, None, false).unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        req.mode = Mode::Resume;
        assert_eq!(
            book.begin(req.clone(), &tx).unwrap_err(),
            "invalid_launch_mode"
        );
        req.mode = Mode::New;
        tx.try_send(Request::Wake).unwrap();
        assert_eq!(book.begin(req.clone(), &tx).unwrap_err(), "launch_busy");
        assert_eq!(
            book.get(req.operation_id).unwrap_err(),
            "operation_unavailable"
        );
        rx.recv().unwrap();
        book.begin(req.clone(), &tx).unwrap();
        let mut another = req.clone();
        another.operation_id = Uuid::new_v4();
        assert_eq!(book.begin(another, &tx).unwrap_err(), "launch_busy");
        assert_eq!(
            book.get(Uuid::new_v4()).unwrap_err(),
            "operation_unavailable"
        );
    }
}
