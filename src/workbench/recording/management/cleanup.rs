//! Durable cleanup intent followed by a same-filesystem quarantine. Paths are
//! derived exclusively from verified UUIDs and pinned private directories.
use super::super::fs::{Directory, Entries, Identity};
use super::{
    model::*,
    scan::{self, Measure},
};
use crate::workbench::config::ConfigHandle;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::File;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};
use uuid::Uuid;
const JOB_LIMIT: usize = 256 * 1024;
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum State {
    Planned,
    Quarantined,
    Deleting,
    Deleted,
    Skipped,
    Failed,
    Cancelled,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Item {
    pub run_epoch: Uuid,
    pub state: State,
    pub reason: Option<String>,
    pub quarantined: bool,
    pub candidate: Option<Candidate>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Job {
    pub schema_version: u32,
    pub job_id: Uuid,
    pub workspace_id: String,
    pub root_identity: Identity,
    pub trash_identity: Identity,
    pub preview_id: Uuid,
    pub config_revision: String,
    pub request_digest: String,
    pub mode: String,
    pub retention_days: Option<u32>,
    pub created_at: String,
    pub cancelled: bool,
    pub items: Vec<Item>,
}
impl Job {
    pub fn pending(&self) -> bool {
        self.items.iter().any(|i| {
            matches!(
                i.state,
                State::Planned | State::Quarantined | State::Deleting
            ) || i.state == State::Failed
        })
    }
    pub fn value(&self) -> Value {
        json!({"jobId":self.job_id,"operationId":self.job_id,"mode":self.mode,"createdAt":self.created_at,"cancelRequested":self.cancelled,
        "status":if self.items.iter().any(|i|i.state==State::Failed){"failed"}else if self.pending(){"pending"}else{"complete"},
        "items":self.items.iter().map(|i|json!({"runEpoch":i.run_epoch,"state":i.state,"reason":i.reason,"pendingCleanup":i.quarantined && i.state!=State::Deleted,"run":i.candidate.as_ref().map(|c|&c.row)})).collect::<Vec<_>>()})
    }
    pub fn matches(&self, preview: Uuid, revision: &str) -> bool {
        self.preview_id == preview
            && self.config_revision == revision
            && self.request_digest == digest(preview, revision)
    }
}
fn digest(preview: Uuid, revision: &str) -> String {
    blake3::hash(format!("{preview}:{revision}").as_bytes())
        .to_hex()
        .to_string()
}
fn fail<T>() -> io::Result<T> {
    Err(io::ErrorKind::InvalidData.into())
}
fn absent(dir: &Directory, name: &str) -> io::Result<bool> {
    match dir.entry_info(name) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Ok(_) => Ok(false),
        Err(e) => Err(e),
    }
}
pub(super) fn read_job(root: &Directory, workspace: &str, id: Uuid) -> io::Result<Job> {
    let job = read_job_any(root, id)?;
    if job.workspace_id != workspace {
        return Err(io::ErrorKind::NotFound.into());
    }
    Ok(job)
}
pub(super) fn read_job_any(root: &Directory, id: Uuid) -> io::Result<Job> {
    let bytes = root
        .dir("cleanup", false)?
        .dir("jobs", false)?
        .read(&format!("{id}.json"), JOB_LIMIT)?;
    let job: Job = serde_json::from_slice(&bytes)?;
    if job.schema_version != 1
        || job.job_id != id
        || job.workspace_id.len() != 64
        || job.root_identity != root.identity()?
        || job.items.len() > 100
        || job.items.is_empty()
        || !matches!(job.mode.as_str(), "manual" | "retention")
        || job.request_digest != digest(job.preview_id, &job.config_revision)
        || (job.mode == "retention" && job.retention_days.is_none_or(|d| !(1..=3650).contains(&d)))
        || job.items.iter().any(|i| {
            i.run_epoch.is_nil()
                || i.candidate.as_ref().is_some_and(|c| {
                    c.row.run_epoch != i.run_epoch
                        || !c.row.manual_eligible
                        || c.row.size.bytes.is_none()
                })
        })
        || job
            .items
            .iter()
            .map(|i| i.run_epoch)
            .collect::<std::collections::HashSet<_>>()
            .len()
            != job.items.len()
    {
        return fail();
    }
    Ok(job)
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Removed {
    schema_version: u32,
    workspace_id: String,
    run_epoch: Uuid,
    job_id: Uuid,
}
pub(crate) fn known_removed(root: &Directory, workspace: &str, epoch: Uuid) -> bool {
    (|| -> io::Result<bool> {
        let marker: Removed = serde_json::from_slice(
            &root
                .dir("cleanup", false)?
                .dir("removed", false)?
                .read(&format!("{epoch}.json"), 1024)?,
        )?;
        if marker.schema_version != 1
            || marker.workspace_id != workspace
            || marker.run_epoch != epoch
        {
            return Ok(false);
        }
        let job = read_job(root, workspace, marker.job_id)?;
        Ok(job
            .items
            .iter()
            .any(|i| i.run_epoch == epoch && (i.quarantined || i.state == State::Deleted)))
    })()
    .unwrap_or(false)
}

pub(super) struct Store {
    root: Directory,
    runs: Directory,
    cleanup: Directory,
    jobs: Directory,
    trash: Directory,
    removed: Directory,
    lock: File,
}
impl Store {
    fn open(path: &Path, identity: Identity) -> io::Result<Self> {
        let root = Directory::root(path)?;
        if root.identity()? != identity {
            return fail();
        }
        let runs = root.dir("runs", false)?;
        let cleanup = root.dir("cleanup", true)?;
        let lock = scan::lock(&cleanup, "lock", true)?;
        let jobs = cleanup.dir("jobs", true)?;
        let trash = cleanup.dir("trash", true)?;
        let removed = cleanup.dir("removed", true)?;
        Ok(Self {
            root,
            runs,
            cleanup,
            jobs,
            trash,
            removed,
            lock,
        })
    }
    fn verify(&self, job: &Job) -> io::Result<()> {
        for d in [
            &self.root,
            &self.runs,
            &self.cleanup,
            &self.jobs,
            &self.trash,
            &self.removed,
        ] {
            d.verify_location()?;
        }
        if self.root.identity()? != job.root_identity
            || self.cleanup.entry_info("lock")?.identity != scan::file_identity(&self.lock)?
        {
            return fail();
        }
        Ok(())
    }
    fn save(&self, job: &Job) -> io::Result<()> {
        self.verify(job)?;
        let bytes = serde_json::to_vec(job)?;
        if bytes.len() > JOB_LIMIT {
            return fail();
        }
        self.jobs.atomic(&format!("{}.json", job.job_id), &bytes)
    }
    fn marker(&self, job: &Job, epoch: Uuid) -> io::Result<()> {
        self.removed.atomic(
            &format!("{epoch}.json"),
            &serde_json::to_vec(&Removed {
                schema_version: 1,
                workspace_id: job.workspace_id.clone(),
                run_epoch: epoch,
                job_id: job.job_id,
            })?,
        )
    }
}

enum Work {
    Check,
    Measuring(usize, Box<Measure>),
    Deleting(usize, Eraser),
}
enum Trigger {
    Manual(Uuid),
    Retention(u32),
}
pub(super) struct Task {
    store: Store,
    pub job: Job,
    work: Work,
    position: usize,
    config: ConfigHandle,
    current: Uuid,
    pub retention_paused: bool,
    #[cfg(test)]
    pub crash: Option<&'static str>,
}
impl Task {
    pub fn create(
        path: &Path,
        workspace: &str,
        current: Uuid,
        config: ConfigHandle,
        p: &Preview,
        revision: &str,
        operation: Uuid,
    ) -> Result<Self, Error> {
        Self::create_with_policy(
            path,
            workspace,
            current,
            config,
            p,
            revision,
            Trigger::Manual(operation),
        )
    }
    pub fn create_automatic(
        path: &Path,
        workspace: &str,
        current: Uuid,
        config: ConfigHandle,
        p: &Preview,
        revision: &str,
        days: u32,
    ) -> Result<Self, Error> {
        Self::create_with_policy(
            path,
            workspace,
            current,
            config,
            p,
            revision,
            Trigger::Retention(days),
        )
    }
    fn create_with_policy(
        path: &Path,
        workspace: &str,
        current: Uuid,
        config: ConfigHandle,
        p: &Preview,
        revision: &str,
        trigger: Trigger,
    ) -> Result<Self, Error> {
        let (operation, retention_days) = match trigger {
            Trigger::Manual(operation) => (operation, None),
            Trigger::Retention(days) => (Uuid::new_v4(), Some(days)),
        };
        if operation.is_nil()
            || p.status != "ready"
            || !p.executable
            || p.mode
                != if retention_days.is_some() {
                    "retention"
                } else {
                    "manual"
                }
            || p.candidates.is_empty()
        {
            return Err(Error::Invalid);
        }
        if p.config_revision.as_deref() != Some(revision)
            || !chrono::DateTime::parse_from_rfc3339(&p.expires_at)
                .is_ok_and(|t| t > chrono::Utc::now())
        {
            return Err(Error::Stale);
        }
        let (policy, actual) = config.history_policy().map_err(|_| Error::Disabled)?;
        if !policy.history.cleanup.enabled {
            return Err(Error::Disabled);
        }
        if retention_days.is_some_and(|days| {
            !policy.history.cleanup.retention.enabled
                || policy.history.cleanup.retention.days != days
        }) {
            return Err(Error::Disabled);
        }
        if actual != revision {
            return Err(Error::Stale);
        }
        let identity = p.root_identity.ok_or(Error::Stale)?;
        let store = Store::open(path, identity).map_err(|_| Error::Busy)?;
        if !absent(&store.jobs, &format!("{operation}.json")).map_err(|_| Error::Unavailable)?
            || !absent(&store.trash, &operation.to_string()).map_err(|_| Error::Unavailable)?
        {
            return Err(Error::Stale);
        }
        let target = store
            .trash
            .dir(&operation.to_string(), true)
            .map_err(|_| Error::Unavailable)?;
        let items = p
            .items
            .iter()
            .map(|i| Item {
                run_epoch: i.run_epoch,
                state: if i.eligible {
                    State::Planned
                } else {
                    State::Skipped
                },
                reason: i.reason.clone(),
                quarantined: false,
                candidate: p
                    .candidates
                    .iter()
                    .find(|c| c.row.run_epoch == i.run_epoch)
                    .cloned(),
            })
            .collect();
        let job = Job {
            schema_version: 1,
            job_id: operation,
            workspace_id: workspace.into(),
            root_identity: identity,
            trash_identity: target.identity().map_err(|_| Error::Unavailable)?,
            preview_id: p.preview_id,
            config_revision: revision.into(),
            request_digest: digest(p.preview_id, revision),
            mode: if retention_days.is_some() {
                "retention"
            } else {
                "manual"
            }
            .into(),
            retention_days,
            created_at: chrono::Utc::now().to_rfc3339(),
            cancelled: false,
            items,
        };
        store.save(&job).map_err(|_| Error::Unavailable)?;
        Ok(Self {
            store,
            job,
            work: Work::Check,
            position: 0,
            config,
            current,
            retention_paused: false,
            #[cfg(test)]
            crash: None,
        })
    }
    pub fn resume(
        path: &Path,
        workspace: &str,
        current: Uuid,
        config: ConfigHandle,
        id: Uuid,
        identity: Identity,
    ) -> Result<Self, Error> {
        let store = Store::open(path, identity).map_err(|_| Error::Busy)?;
        let job = read_job(&store.root, workspace, id).map_err(|_| Error::NotFound)?;
        let task = Self {
            store,
            job,
            work: Work::Check,
            position: 0,
            config,
            current,
            retention_paused: false,
            #[cfg(test)]
            crash: None,
        };
        if !task.policy_allows()? {
            return Err(Error::Disabled);
        }
        Ok(task)
    }
    fn policy_allows(&self) -> Result<bool, Error> {
        if self.job.mode == "retention" && self.retention_paused {
            return Ok(false);
        }
        let (config, _) = self.config.history_policy().map_err(|_| Error::Disabled)?;
        Ok(config.history.cleanup.enabled
            && (self.job.mode != "retention" || config.history.cleanup.retention.enabled))
    }
    fn due(&self, c: &Candidate) -> bool {
        if self.job.mode != "retention" {
            return true;
        }
        self.config
            .history_policy()
            .ok()
            .is_some_and(|(config, _)| {
                config.history.cleanup.enabled
                    && config.history.cleanup.retention.enabled
                    && scan::expired(
                        &c.row,
                        config.history.cleanup.retention.days,
                        chrono::Utc::now(),
                    )
            })
    }
    pub fn cancel(&mut self) -> Result<(), Error> {
        self.job.cancelled = true;
        for item in &mut self.job.items {
            if item.state == State::Planned {
                item.state = State::Cancelled;
                item.reason = Some("cancelled".into());
            }
        }
        self.store.save(&self.job).map_err(|_| Error::Unavailable)
    }
    pub fn record_failure(&mut self) {
        let Some(item) = self.job.items.get_mut(self.position) else {
            return;
        };
        if matches!(
            item.state,
            State::Deleted | State::Skipped | State::Cancelled
        ) {
            return;
        }
        if let Some(c) = &item.candidate
            && self
                .store
                .trash
                .dir(&self.job.job_id.to_string(), false)
                .and_then(|d| d.entry_info(&item.run_epoch.to_string()))
                .is_ok_and(|info| info.directory && info.identity == c.identity)
        {
            item.quarantined = true;
        }
        item.state = State::Failed;
        item.reason = Some("storage_or_identity_error".into());
        let _ = self.store.save(&self.job);
    }
    fn finish_item(&mut self, index: usize, state: State, reason: Option<&str>) -> io::Result<()> {
        self.job.items[index].state = state;
        self.job.items[index].reason = reason.map(str::to_owned);
        self.store.save(&self.job)?;
        if state == State::Deleted {
            if let Ok(cache) = self.store.root.dir("usage-v1", false) {
                let _ = cache.remove(&format!("{}.json", self.job.items[index].run_epoch));
            }
            if self.store.root.open("index.sqlite", false).is_ok() {
                let _ = self.store.root.remove("index.sqlite");
            }
        }
        self.position = index + 1;
        self.work = Work::Check;
        Ok(())
    }
    fn checkpoint(&self, point: &str) -> io::Result<()> {
        #[cfg(test)]
        if self.crash == Some(point) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let _ = point;
        Ok(())
    }
    /// true means this attempt is finished, including a policy pause. A later
    /// worker can resume a verified intent only while deletion remains enabled.
    pub fn step(&mut self) -> io::Result<bool> {
        self.store.verify(&self.job)?;
        if self
            .store
            .trash
            .dir(&self.job.job_id.to_string(), false)?
            .identity()?
            != self.job.trash_identity
        {
            return fail();
        }
        let work = std::mem::replace(&mut self.work, Work::Check);
        match work {
            Work::Check => {
                if !self.policy_allows().unwrap_or(false) {
                    return Ok(true);
                }
                while self.position < self.job.items.len() {
                    let index = self.position;
                    let item = &self.job.items[index];
                    if matches!(
                        item.state,
                        State::Deleted | State::Skipped | State::Cancelled
                    ) {
                        self.position += 1;
                        continue;
                    }
                    if item.run_epoch == self.current {
                        self.finish_item(index, State::Skipped, Some("current_run"))?;
                        continue;
                    }
                    let id = item.run_epoch;
                    let Some(c) = item.candidate.clone() else {
                        return fail();
                    };
                    let target = self.store.trash.dir(&self.job.job_id.to_string(), false)?;
                    if target.identity()? != self.job.trash_identity {
                        return fail();
                    }
                    let source_missing = absent(&self.store.runs, &id.to_string())?;
                    let target_missing = absent(&target, &id.to_string())?;
                    if !source_missing && !target_missing {
                        self.finish_item(index, State::Failed, Some("identity_conflict"))?;
                        continue;
                    }
                    if !target_missing {
                        // Rename can be durable even when its following job update
                        // was interrupted. Verify the exact inode from the intent.
                        let dir = target.dir(&id.to_string(), false)?;
                        if dir.identity()? != c.identity {
                            return fail();
                        }
                        if !self.due(&c) {
                            return Ok(true);
                        }
                        let eraser = Eraser::resume(
                            dir,
                            &c,
                            item.state == State::Deleting || item.quarantined,
                        )?;
                        self.job.items[index].quarantined = true;
                        self.job.items[index].state = State::Deleting;
                        self.store.save(&self.job)?;
                        self.store.marker(&self.job, id)?;
                        self.work = Work::Deleting(index, eraser);
                        return Ok(false);
                    }
                    if source_missing {
                        if item.quarantined && matches!(item.state, State::Deleting | State::Failed)
                        {
                            self.finish_item(index, State::Deleted, None)?;
                        } else {
                            self.finish_item(index, State::Skipped, Some("history_missing"))?;
                        }
                        continue;
                    }
                    if item.quarantined {
                        return fail();
                    }
                    if self.job.cancelled {
                        self.finish_item(index, State::Cancelled, Some("cancelled"))?;
                        continue;
                    }
                    match Measure::begin(&self.store.runs, id, &self.job.workspace_id, self.current)
                    {
                        Ok(Some(m)) => {
                            self.work = Work::Measuring(index, Box::new(m));
                            return Ok(false);
                        }
                        _ => {
                            self.finish_item(
                                index,
                                State::Skipped,
                                Some("identity_changed_or_unreadable"),
                            )?;
                            continue;
                        }
                    }
                }
                Ok(true)
            }
            Work::Measuring(index, mut measure) => {
                if !measure.step(128)? {
                    self.work = Work::Measuring(index, measure);
                    return Ok(false);
                }
                let (now, dir, lock) = (*measure).finish_locked();
                let expected = self.job.items[index]
                    .candidate
                    .as_ref()
                    .ok_or(io::ErrorKind::InvalidData)?;
                let unchanged = now.identity == expected.identity
                    && now.lock_identity == expected.lock_identity
                    && now.meta_digest == expected.meta_digest
                    && now.contents_digest == expected.contents_digest
                    && now.file_count == expected.file_count;
                if !now.row.manual_eligible || !unchanged || lock.is_none() {
                    self.finish_item(
                        index,
                        State::Skipped,
                        Some(if now.row.manual_eligible {
                            "preview_changed"
                        } else {
                            "active_or_unsafe_run"
                        }),
                    )?;
                    return Ok(false);
                }
                if !self.policy_allows().unwrap_or(false) {
                    return Ok(true);
                }
                if self.job.cancelled {
                    self.finish_item(index, State::Cancelled, Some("cancelled"))?;
                    return Ok(false);
                }
                if !self.due(&now) {
                    self.finish_item(index, State::Skipped, Some("not_expired"))?;
                    return Ok(false);
                }
                self.checkpoint("before_rename")?;
                let target = self.store.trash.dir(&self.job.job_id.to_string(), false)?;
                if target.identity()? != self.job.trash_identity {
                    return fail();
                }
                self.store.runs.move_directory(
                    &now.row.run_epoch.to_string(),
                    &target,
                    now.identity,
                )?;
                self.checkpoint("after_rename")?;
                drop(dir);
                let moved = target.dir(&now.row.run_epoch.to_string(), false)?;
                if moved.identity()? != now.identity {
                    return fail();
                }
                self.job.items[index].quarantined = true;
                self.job.items[index].state = State::Quarantined;
                self.store.save(&self.job)?;
                self.store.marker(&self.job, now.row.run_epoch)?;
                self.checkpoint("after_quarantine")?;
                let eraser = Eraser::new(moved, lock)?;
                self.job.items[index].state = State::Deleting;
                self.store.save(&self.job)?;
                self.work = Work::Deleting(index, eraser);
                Ok(false)
            }
            Work::Deleting(index, mut eraser) => {
                // One quarantined run is the irreversible unit. Settings/cancel
                // changes take effect before the next run, never halfway through it.
                self.checkpoint("before_delete")?;
                if !eraser.step(128)? {
                    self.work = Work::Deleting(index, eraser);
                    self.checkpoint("during_delete")?;
                    return Ok(false);
                }
                self.checkpoint("before_metadata_delete")?;
                for name in ["meta.json", "lock"] {
                    if !absent(&eraser.dir, name)? {
                        eraser.dir.remove(name)?;
                    }
                }
                eraser.dir.sync()?;
                self.checkpoint("after_metadata_delete")?;
                let target = self.store.trash.dir(&self.job.job_id.to_string(), false)?;
                target.remove_dir(
                    &self.job.items[index].run_epoch.to_string(),
                    self.job.items[index].candidate.as_ref().unwrap().identity,
                )?;
                self.checkpoint("after_directory_delete")?;
                self.finish_item(index, State::Deleted, None)?;
                Ok(false)
            }
        }
    }
}

pub(super) fn cancel_job(
    path: &Path,
    workspace: &str,
    id: Uuid,
    identity: Identity,
) -> Result<Job, Error> {
    let store = Store::open(path, identity).map_err(|_| Error::Busy)?;
    let mut job = read_job(&store.root, workspace, id).map_err(|_| Error::NotFound)?;
    job.cancelled = true;
    for item in &mut job.items {
        if item.state == State::Planned {
            item.state = State::Cancelled;
            item.reason = Some("cancelled".into());
        }
    }
    store.save(&job).map_err(|_| Error::Unavailable)?;
    Ok(job)
}

struct Eraser {
    dir: Directory,
    entries: Entries,
    blobs: Option<(Directory, Entries)>,
    _lock: Option<File>,
}
impl Eraser {
    fn new(dir: Directory, lock: Option<File>) -> io::Result<Self> {
        Ok(Self {
            entries: dir.entries()?,
            dir,
            blobs: None,
            _lock: lock,
        })
    }
    fn resume(dir: Directory, c: &Candidate, may_be_partial: bool) -> io::Result<Self> {
        if dir.identity()? != c.identity {
            return fail();
        }
        let missing_meta = absent(&dir, "meta.json")?;
        let missing_lock = absent(&dir, "lock")?;
        if missing_meta && !may_be_partial {
            return fail();
        }
        if !missing_meta
            && blake3::hash(&dir.read("meta.json", 4 * 1024 * 1024)?)
                .to_hex()
                .as_str()
                != c.meta_digest
        {
            return fail();
        }
        let lock = if !missing_lock {
            let lock = scan::lock(&dir, "lock", false)?;
            if scan::file_identity(&lock)? != c.lock_identity {
                return fail();
            }
            Some(lock)
        } else {
            if !may_be_partial || dir.entries()?.next().is_some() {
                return fail();
            }
            None
        };
        Self::new(dir, lock)
    }
    fn step(&mut self, budget: usize) -> io::Result<bool> {
        self.dir.verify_location()?;
        let deadline = Instant::now() + Duration::from_millis(20);
        for _ in 0..budget {
            if Instant::now() >= deadline {
                return Ok(false);
            }
            let (dir, next, blobs) = if let Some((d, e)) = &mut self.blobs {
                (&*d, e.next(), true)
            } else {
                (&self.dir, self.entries.next(), false)
            };
            let Some(next) = next else {
                if blobs {
                    let (d, _) = self.blobs.take().unwrap();
                    d.sync()?;
                    self.dir.remove_dir("blobs", d.identity()?)?;
                    continue;
                }
                return Ok(true);
            };
            let name = next?;
            let info = dir.entry_info(&name)?;
            if info.identity.device != self.dir.identity()?.device {
                return fail();
            }
            if info.directory {
                if blobs || name != "blobs" {
                    return fail();
                }
                let dir = dir.dir(&name, false)?;
                let entries = dir.entries()?;
                self.blobs = Some((dir, entries));
            } else {
                if !scan::allowed_file(&name, blobs) {
                    return fail();
                }
                if !blobs && matches!(name.as_str(), "meta.json" | "lock") {
                    continue;
                }
                dir.remove(&name)?;
            }
        }
        Ok(false)
    }
}
