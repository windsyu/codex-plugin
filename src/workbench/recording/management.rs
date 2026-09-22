//! Current-project storage management; no disk I/O enters proxy, PTY or HTTP tasks.
use crate::workbench::config::ConfigHandle;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use uuid::Uuid;
mod cleanup;
mod extras;
mod jobs;
mod model;
mod preview;
mod retention;
mod scan;
pub(super) use cleanup::known_removed;
pub use model::{Error, PreviewMode};
pub(super) use scan::cached;

pub enum Query {
    #[cfg(test)]
    HoldWorker {
        entered: oneshot::Sender<()>,
        release: mpsc::Receiver<()>,
    },
    Usage {
        cursor: Option<String>,
    },
    Refresh,
    Preview {
        mode: PreviewMode,
    },
    ReadPreview {
        id: Uuid,
    },
    CreateJob {
        preview: Uuid,
        revision: String,
        operation: Uuid,
    },
    Job {
        id: Uuid,
    },
    Jobs {
        cursor: Option<String>,
    },
    Cancel {
        id: Uuid,
    },
}
struct Message {
    query: Query,
    reply: oneshot::Sender<Result<Value, Error>>,
}
#[derive(Clone)]
pub struct Handle {
    sender: mpsc::SyncSender<Message>,
}
impl Handle {
    pub async fn query(&self, query: Query) -> Result<Value, Error> {
        let (reply, response) = oneshot::channel();
        self.sender
            .try_send(Message { query, reply })
            .map_err(|_| Error::Busy)?;
        tokio::time::timeout(Duration::from_secs(3), response)
            .await
            .map_err(|_| Error::Busy)?
            .map_err(|_| Error::Unavailable)?
    }
}
pub struct Service {
    handle: Handle,
    stop: Arc<AtomicBool>,
}
impl Service {
    pub fn start(
        root: PathBuf,
        workspace: String,
        current: Uuid,
        config: ConfigHandle,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Message>(8);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        std::thread::Builder::new()
            .name("workbench-history-management".into())
            .spawn(move || {
                let mut engine = Engine::new(root, workspace, current, config);
                while !stopping.load(Ordering::Acquire) {
                    match receiver.recv_timeout(Duration::from_millis(10)) {
                        Ok(message) => {
                            if !message.reply.is_closed() {
                                let value = engine.query(message.query);
                                let _ = message.reply.send(value);
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(_) => {}
                    }
                    engine.tick();
                }
            })?;
        Ok(Self {
            handle: Handle { sender },
            stop,
        })
    }
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

struct Engine {
    root: PathBuf,
    workspace: String,
    current: Uuid,
    config: ConfigHandle,
    scan: Option<scan::Scan>,
    scan_requested: bool,
    retry: Instant,
    previews: HashMap<Uuid, model::Preview>,
    waiting: VecDeque<(Uuid, PreviewMode)>,
    building: Option<preview::Builder>,
    root_identity: Option<super::fs::Identity>,
    cleanup: Option<cleanup::Task>,
    listing: Option<jobs::Cursor>,
    recovery: Option<jobs::Cursor>,
    recovery_at: Instant,
    retention: retention::Scheduler,
}
impl Engine {
    fn new(root: PathBuf, workspace: String, current: Uuid, config: ConfigHandle) -> Self {
        Self {
            root,
            workspace,
            current,
            config,
            scan: None,
            scan_requested: true,
            retry: Instant::now(),
            previews: HashMap::new(),
            waiting: VecDeque::new(),
            building: None,
            root_identity: None,
            cleanup: None,
            listing: None,
            recovery: None,
            recovery_at: Instant::now(),
            retention: retention::Scheduler::new(),
        }
    }
    fn query(&mut self, query: Query) -> Result<Value, Error> {
        let mut value = match query {
            #[cfg(test)]
            Query::HoldWorker { entered, release } => {
                let _ = entered.send(());
                let _ = release.recv_timeout(Duration::from_secs(10));
                json!({"released":true})
            }
            Query::Usage { cursor } => {
                let mut value = json!(
                    self.scan
                        .as_mut()
                        .ok_or(Error::Busy)?
                        .page(cursor.as_deref())?
                );
                value["retention"] = self.retention.value();
                value
            }
            Query::Refresh => {
                if self
                    .scan
                    .as_ref()
                    .is_none_or(|s| s.info.state != "scanning")
                {
                    self.scan_requested = true;
                }
                json!({"scheduled":true})
            }
            Query::Preview { mode } => {
                match &mode {
                    PreviewMode::Manual(ids)
                        if ids.is_empty()
                            || ids.len() > 100
                            || ids.iter().any(Uuid::is_nil)
                            || ids.iter().collect::<std::collections::HashSet<_>>().len()
                                != ids.len() =>
                    {
                        return Err(Error::Invalid);
                    }
                    PreviewMode::Retention(days) if !(1..=3650).contains(days) => {
                        return Err(Error::Invalid);
                    }
                    _ => {}
                }
                self.previews.retain(|_, p| {
                    p.status == "queued"
                        || p.status == "scanning"
                        || chrono::DateTime::parse_from_rfc3339(&p.expires_at)
                            .is_ok_and(|t| t > chrono::Utc::now())
                });
                if self.previews.len() >= 16 {
                    return Err(Error::Busy);
                }
                let id = Uuid::new_v4();
                self.previews.insert(
                    id,
                    model::Preview {
                        preview_id: id,
                        status: "queued".into(),
                        config_revision: None,
                        expires_at: chrono::Utc::now().to_rfc3339(),
                        mode: match &mode {
                            PreviewMode::Manual(_) => "manual",
                            PreviewMode::Retention(_) => "retention_draft",
                        }
                        .into(),
                        executable: false,
                        items: vec![],
                        candidates: vec![],
                        root_identity: None,
                        scan_complete: false,
                        error: None,
                        skipped_counts: Default::default(),
                    },
                );
                self.waiting.push_back((id, mode));
                json!({"previewId":id})
            }
            Query::ReadPreview { id } => {
                let p = self.previews.get(&id).ok_or(Error::NotFound)?;
                if p.status == "ready"
                    && chrono::DateTime::parse_from_rfc3339(&p.expires_at)
                        .is_ok_and(|t| t <= chrono::Utc::now())
                {
                    return Err(Error::Stale);
                }
                json!(p)
            }
            Query::CreateJob {
                preview,
                revision,
                operation,
            } => {
                let root = jobs::root(&self.root, self.root_identity)?;
                if let Ok(job) = cleanup::read_job(&root, &self.workspace, operation) {
                    if !job.matches(preview, &revision) {
                        return Err(Error::Stale);
                    }
                    job.value()
                } else {
                    if self.cleanup.is_some() || self.building.is_some() {
                        return Err(Error::Busy);
                    }
                    let (config, _) = self.config.history_policy().map_err(|_| Error::Disabled)?;
                    if !config.history.cleanup.enabled {
                        return Err(Error::Disabled);
                    }
                    let p = self.previews.get(&preview).ok_or(Error::NotFound)?;
                    if p.root_identity != self.root_identity {
                        return Err(Error::Stale);
                    }
                    if let Some(scan) = &mut self.scan {
                        scan.pause();
                    }
                    let task = cleanup::Task::create(
                        &self.root,
                        &self.workspace,
                        self.current,
                        self.config.clone(),
                        p,
                        &revision,
                        operation,
                    )?;
                    let value = task.job.value();
                    self.cleanup = Some(task);
                    self.listing = None;
                    value
                }
            }
            Query::Job { id } => cleanup::read_job(
                &jobs::root(&self.root, self.root_identity)?,
                &self.workspace,
                id,
            )
            .map_err(|_| Error::NotFound)?
            .value(),
            Query::Jobs { cursor } => {
                if cursor.is_none() {
                    self.listing =
                        match jobs::Cursor::new(jobs::root(&self.root, self.root_identity)?) {
                            Ok(cursor) => Some(cursor),
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                            Err(_) => return Err(Error::Unavailable),
                        };
                }
                if let Some(listing) = &mut self.listing {
                    listing.page(&self.workspace, cursor.as_deref())?
                } else if cursor.is_some() {
                    return Err(Error::Stale);
                } else {
                    json!({"jobs":[],"nextCursor":null,"unverifiedEntries":0})
                }
            }
            Query::Cancel { id } => {
                self.listing = None;
                if let Some(task) = &mut self.cleanup {
                    if task.job.job_id == id {
                        task.cancel()?;
                        task.job.value()
                    } else {
                        return Err(Error::Busy);
                    }
                } else {
                    cleanup::cancel_job(
                        &self.root,
                        &self.workspace,
                        id,
                        self.root_identity.ok_or(Error::Busy)?,
                    )?
                    .value()
                }
            }
        };
        if value.get("jobId").is_some() || value.get("jobs").is_some() {
            let policy = self.config.history_policy();
            let decorate = |job: &mut Value| {
                if job["status"] == "pending" {
                    if job["mode"] == "retention" && self.retention.clock_blocked {
                        job["status"] = json!("paused");
                        job["pauseReason"] = json!("clock_changed");
                        return;
                    }
                    let permitted = policy.as_ref().is_ok_and(|(c, _)| {
                        c.history.cleanup.enabled
                            && (job["mode"] != "retention" || c.history.cleanup.retention.enabled)
                    });
                    if !permitted {
                        job["status"] = json!("paused");
                        job["pauseReason"] = json!(if policy.is_err() {
                            "config_unavailable"
                        } else {
                            "cleanup_disabled"
                        });
                    }
                }
            };
            if value.get("jobId").is_some() {
                decorate(&mut value);
            }
            if let Some(jobs) = value.get_mut("jobs").and_then(Value::as_array_mut) {
                for job in jobs {
                    decorate(job);
                }
            }
        }
        Ok(json!({"currentRunEpoch":self.current,"result":value}))
    }
    fn tick(&mut self) {
        if Instant::now() >= self.retention.policy_at {
            self.retention.policy_at = Instant::now() + Duration::from_secs(1);
            self.retention
                .observe_clock(Instant::now(), chrono::Utc::now());
            if self.retention.policy(self.config.history_policy().ok()) {
                self.recovery_at = Instant::now();
                self.recovery = None;
            }
        }
        if let Some(task) = &mut self.cleanup {
            task.retention_paused = self.retention.clock_blocked;
            let result = task.step();
            if result.is_err() {
                task.record_failure();
            }
            match result {
                Ok(false) => return,
                _ => {
                    self.cleanup = None;
                    self.scan = None;
                    self.scan_requested = true;
                    self.listing = None;
                    self.recovery_at = Instant::now() + Duration::from_secs(60);
                }
            }
        }
        if self.scan_requested && self.building.is_none() && Instant::now() >= self.retry {
            // Release the previous scan's lock before replacing its ephemeral pages.
            if self
                .scan
                .as_ref()
                .is_none_or(|s| s.info.state != "scanning")
            {
                match scan::Scan::begin(&self.root) {
                    Ok(scan) => {
                        let identity = scan.root.identity().ok();
                        if self.root_identity.is_some() && identity != self.root_identity {
                            self.retry = Instant::now() + Duration::from_secs(1);
                            return;
                        }
                        self.root_identity = identity;
                        self.scan = Some(scan);
                        self.scan_requested = false;
                    }
                    Err(_) => self.retry = Instant::now() + Duration::from_secs(1),
                }
            }
        }
        if self.building.is_none()
            && let Some((id, mode)) = self.waiting.pop_front()
        {
            if let Some(scan) = &mut self.scan {
                scan.pause();
            }
            let p = self.previews.get_mut(&id).unwrap();
            match preview::Builder::begin(
                &self.root,
                mode,
                self.scan.as_ref().is_some_and(|s| s.scan_lock.is_some()),
            ) {
                Ok(builder) => {
                    if self.root_identity.is_some() && builder.identity().ok() != self.root_identity
                    {
                        p.status = "failed".into();
                        p.error = Some("history_unavailable".into());
                        return;
                    }
                    self.root_identity = builder.identity().ok();
                    p.status = "scanning".into();
                    p.root_identity = builder.identity().ok();
                    match self.config.history_policy() {
                        Ok((c, r)) => {
                            p.config_revision = Some(r);
                            p.executable = p.mode == "manual" && c.history.cleanup.enabled;
                        }
                        Err(_) => {
                            p.error = Some("config_unavailable".into());
                        }
                    }
                    self.building = Some(builder.with_id(id));
                }
                Err(_) => {
                    p.status = "failed".into();
                    p.error = Some("history_busy_or_unavailable".into());
                    p.expires_at = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
                }
            }
        }
        if let Some(builder) = &mut self.building {
            let p = self.previews.get_mut(&builder.id).unwrap();
            match builder.step(&self.workspace, self.current, p) {
                Ok(false) => {}
                result => {
                    p.status = if result.is_ok() { "ready" } else { "failed" }.into();
                    if result.is_err() {
                        p.error = Some("history_unavailable".into());
                        p.executable = false;
                    }
                    if self.config.history_policy().ok().map(|(_, r)| r) != p.config_revision {
                        p.executable = false;
                        p.error = Some("config_changed".into());
                    }
                    p.expires_at = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
                    self.building = None;
                }
            }
        } else if let Some(scan) = &mut self.scan
            && scan.info.state == "scanning"
            && scan.step(&self.workspace, self.current).is_err()
        {
            scan.info.state = "unavailable".into();
            scan.scan_lock = None;
        }
        if self.building.is_none() && self.waiting.is_empty() && self.root_identity.is_some() {
            if self.recovery.is_none() && Instant::now() >= self.recovery_at {
                self.recovery_at = Instant::now() + Duration::from_secs(60);
                if self
                    .config
                    .history_policy()
                    .is_ok_and(|(c, _)| c.history.cleanup.enabled)
                {
                    self.recovery = jobs::root(&self.root, self.root_identity)
                        .ok()
                        .and_then(|root| jobs::Cursor::new(root).ok());
                }
            }
            if let Some(cursor) = &mut self.recovery {
                match cursor.next_pending(&self.workspace) {
                    Ok((_, Some(job))) => {
                        if let Some(scan) = &mut self.scan {
                            scan.pause();
                        }
                        self.cleanup = cleanup::Task::resume(
                            &self.root,
                            &self.workspace,
                            self.current,
                            self.config.clone(),
                            job.job_id,
                            self.root_identity.unwrap(),
                        )
                        .ok();
                    }
                    Ok((true, _)) | Err(_) => self.recovery = None,
                    _ => {}
                }
            }
            if self.cleanup.is_none() && self.retention.enabled && !self.retention.clock_blocked {
                if let Some(id) = self.retention.preview {
                    if let Some(p) = self.previews.get(&id) {
                        if p.status == "ready" {
                            let mut p = p.clone();
                            self.retention.skipped = p.skipped_counts.clone();
                            self.retention.scan_complete = Some(p.scan_complete);
                            p.mode = "retention".into();
                            p.executable = true;
                            if p.config_revision != self.retention.revision {
                                self.retention.retry("config_changed");
                            } else if p.candidates.is_empty() {
                                self.retention.complete(None);
                            } else {
                                if let Some(scan) = &mut self.scan {
                                    scan.pause();
                                }
                                match cleanup::Task::create_automatic(
                                    &self.root,
                                    &self.workspace,
                                    self.current,
                                    self.config.clone(),
                                    &p,
                                    p.config_revision.as_deref().unwrap(),
                                    self.retention.days,
                                ) {
                                    Ok(task) => {
                                        self.retention.complete(Some(task.job.job_id));
                                        self.cleanup = Some(task);
                                    }
                                    Err(error) => self.retention.retry(match error {
                                        Error::Busy => "history_busy_or_unavailable",
                                        Error::Stale | Error::Disabled => "config_changed",
                                        _ => "history_unavailable",
                                    }),
                                }
                            }
                        } else if p.status == "failed" {
                            self.retention.retry("history_busy_or_unavailable");
                        }
                    } else {
                        self.retention.retry("history_unavailable");
                    }
                } else if Instant::now() >= self.retention.due {
                    match self.query(Query::Preview {
                        mode: PreviewMode::Retention(self.retention.days),
                    }) {
                        Ok(v) => {
                            self.retention.preview =
                                serde_json::from_value(v["result"]["previewId"].clone()).ok();
                            self.retention.state = "checking";
                        }
                        Err(_) => self.retention.retry("history_busy_or_unavailable"),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
