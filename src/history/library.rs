//! Derived history catalog. All filesystem/SQLite work stays on bounded workers.
pub(crate) mod launch;
mod model;
mod native;
#[cfg(test)]
mod scale_browser;
mod scan;
#[cfg(test)]
mod tests;
mod window;
use crate::workbench::{config::ConfigHandle, recording::fs::Directory};
use model::*;
pub use model::{LibraryConfig, Page, Query, Source, SourceStatus};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use scan::{Scan, Step};
use serde_json::{Value, json};
use std::os::fd::AsRawFd;
use std::{
    path::PathBuf,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

// Version 2 separates execution cwd from display projects. Only this derived cache is rebuilt.
const CATALOG_VERSION: i64 = 2;

type Result<T> = std::result::Result<T, &'static str>;
type Policy = Arc<dyn Fn() -> Result<LibraryConfig> + Send + Sync>;
struct Shared {
    home: PathBuf,
    data: PathBuf,
    policy: Policy,
    status: RwLock<Vec<SourceStatus>>,
    stop: AtomicBool,
    refresh: AtomicBool,
    key: [u8; 32],
}
impl Shared {
    fn policy(&self) -> Result<(LibraryConfig, Vec<Source>)> {
        let policy = (self.policy)()?;
        let sources = policy.resolved(&self.home, &self.data)?;
        Ok((policy, sources))
    }
    fn states(&self, sources: &[Source]) -> Vec<SourceStatus> {
        let statuses = self.status.read().unwrap();
        sources
            .iter()
            .map(|source| {
                statuses
                    .iter()
                    .find(|s| s.id == source.id() && s.identity == source.identity())
                    .cloned()
                    .unwrap_or_else(|| status(source, "indexing", "正在读取历史目录…", 0, None))
            })
            .collect()
    }
    fn set(&self, mut value: SourceStatus) {
        let mut values = self.status.write().unwrap();
        if let Some(old) = values.iter_mut().find(|v| v.id == value.id) {
            if value.state == "indexing"
                && value.revision.is_none()
                && value.identity == old.identity
            {
                value.revision = old.revision.clone();
            }
            *old = value;
        } else {
            values.push(value);
        }
    }
}
fn status(
    source: &Source,
    state: &str,
    message: &str,
    count: u64,
    revision: Option<String>,
) -> SourceStatus {
    SourceStatus {
        id: source.id().into(),
        identity: source.identity(),
        kind: source.kind().into(),
        state: state.into(),
        message: message.into(),
        indexed_entries: count,
        revision,
        error: None,
    }
}
#[derive(Clone)]
pub(crate) struct LibraryHandle {
    shared: Arc<Shared>,
    sender: mpsc::SyncSender<Request>,
}
enum Operation {
    Sources,
    List(Query, bool),
    Body(String, Query),
    Details(String, Query),
    Preview(PathBuf),
    Refresh,
    Launch(Option<String>, Option<(String, String)>),
}
struct Request {
    operation: Operation,
    reply: oneshot::Sender<Result<Value>>,
}
impl LibraryHandle {
    pub async fn launch_source(
        &self,
        project: Option<String>,
        entry: Option<(String, String)>,
    ) -> Result<launch::Selection> {
        let value = self.call(Operation::Launch(project, entry)).await?;
        serde_json::from_value(value).map_err(|_| "cache_invalid")
    }
    async fn call(&self, operation: Operation) -> Result<Value> {
        let (reply, rx) = oneshot::channel();
        self.sender
            .try_send(Request { operation, reply })
            .map_err(|_| "library_busy")?;
        tokio::time::timeout(Duration::from_secs(3), rx)
            .await
            .map_err(|_| "library_timeout")?
            .map_err(|_| "library_unavailable")?
    }
    pub async fn sources(&self) -> Result<Value> {
        self.call(Operation::Sources).await
    }
    pub async fn list(&self, query: Query, projects: bool) -> Result<Value> {
        self.call(Operation::List(query, projects)).await
    }
    pub async fn body(&self, id: String, query: Query) -> Result<Value> {
        self.call(Operation::Body(id, query)).await
    }
    pub async fn details(&self, id: String, query: Query) -> Result<Value> {
        self.call(Operation::Details(id, query)).await
    }
    pub async fn preview(&self, path: PathBuf) -> Result<Value> {
        self.call(Operation::Preview(path)).await
    }
    pub async fn refresh(&self) -> Result<Value> {
        self.call(Operation::Refresh).await
    }
    pub fn statuses(&self) -> Vec<SourceStatus> {
        self.shared.status.read().unwrap().clone()
    }
}
pub(crate) struct HistoryLibrary {
    handle: LibraryHandle,
    threads: Vec<std::thread::JoinHandle<()>>,
}
impl HistoryLibrary {
    pub fn start(home: PathBuf, data: PathBuf, settings: ConfigHandle) -> std::io::Result<Self> {
        Self::with_policy(
            home,
            data,
            Arc::new(move || {
                settings
                    .history_policy()
                    .map(|v| v.0.history.library)
                    .map_err(|_| "settings_invalid")
            }),
        )
    }
    fn with_policy(home: PathBuf, data: PathBuf, policy: Policy) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            home,
            data,
            policy,
            status: RwLock::new(vec![]),
            stop: AtomicBool::new(false),
            refresh: AtomicBool::new(true),
            key: rand::random(),
        });
        let (sender, receiver) = mpsc::sync_channel::<Request>(16);
        let index = shared.clone();
        let reader = shared.clone();
        let index_thread = std::thread::Builder::new()
            .name("history-library-index".into())
            .spawn(move || index_worker(&index))?;
        let query_thread = std::thread::Builder::new()
            .name("history-library-query".into())
            .spawn(move || {
                while !reader.stop.load(Ordering::Relaxed) {
                    let Ok(request) = receiver.recv_timeout(Duration::from_millis(100)) else {
                        continue;
                    };
                    if request.reply.is_closed() {
                        continue;
                    }
                    let result = query(&reader, request.operation);
                    let _ = request.reply.send(result);
                }
            })?;
        Ok(Self {
            handle: LibraryHandle { shared, sender },
            threads: vec![index_thread, query_thread],
        })
    }
    pub fn handle(&self) -> LibraryHandle {
        self.handle.clone()
    }
}
impl Drop for HistoryLibrary {
    fn drop(&mut self) {
        self.handle.shared.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
fn db_path(shared: &Shared) -> PathBuf {
    shared.data.join("library/catalog.sqlite")
}
fn open_cache(shared: &Shared) -> Result<(Directory, std::fs::File, Connection)> {
    let root = Directory::root(&shared.data).map_err(|_| "cache_unavailable")?;
    let directory = root.dir("library", true).map_err(|_| "cache_unavailable")?;
    let lock = directory
        .open("writer.lock", true)
        .or_else(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                directory.open("writer.lock", false)
            } else {
                Err(e)
            }
        })
        .map_err(|_| "cache_unavailable")?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0 {
        return Err("cache_read_only");
    }
    let _file = directory
        .open("catalog.sqlite", true)
        .or_else(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                directory.open("catalog.sqlite", false)
            } else {
                Err(e)
            }
        })
        .map_err(|_| "cache_unavailable")?;
    for name in [
        "catalog.sqlite-journal",
        "catalog.sqlite-wal",
        "catalog.sqlite-shm",
    ] {
        match directory.open(name, false) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("unsafe_cache"),
        }
    }
    let db = Connection::open(db_path(shared)).map_err(|_| "cache_unavailable")?;
    db.busy_timeout(Duration::from_millis(80))
        .map_err(|_| "cache_unavailable")?;
    let version: i64 = match db.pragma_query_value(None, "user_version", |row| row.get(0)) {
        Ok(version) => version,
        Err(error)
            if matches!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
            ) =>
        {
            0
        }
        Err(_) => return Err("cache_invalid"),
    };
    if !(0..=CATALOG_VERSION).contains(&version) {
        return Err("cache_version_unsupported");
    }
    if version == 1 {
        db.execute_batch("BEGIN IMMEDIATE; DROP TABLE IF EXISTS catalog_checkpoints; DROP TABLE IF EXISTS catalog_locators; DROP TABLE IF EXISTS catalog_records; DROP TABLE IF EXISTS catalog_entries; DROP TABLE IF EXISTS catalog_sources; PRAGMA user_version=0; COMMIT;")
            .map_err(|_| "cache_rebuilding")?;
    }
    let initialize = db.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA cache_size=-4096; PRAGMA max_page_count=262144;
        CREATE TABLE IF NOT EXISTS catalog_sources(id TEXT PRIMARY KEY,identity TEXT NOT NULL,generation TEXT NOT NULL,revision TEXT NOT NULL,issues TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS catalog_entries(generation TEXT NOT NULL,id TEXT NOT NULL,source TEXT NOT NULL,project TEXT,kind TEXT NOT NULL,time TEXT NOT NULL,metadata TEXT NOT NULL,search TEXT NOT NULL,PRIMARY KEY(generation,id));
        CREATE INDEX IF NOT EXISTS catalog_order ON catalog_entries(generation,time,id);
        CREATE TABLE IF NOT EXISTS catalog_records(generation TEXT NOT NULL,entry TEXT NOT NULL,seq INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(generation,entry,seq));
        CREATE TABLE IF NOT EXISTS catalog_locators(generation TEXT NOT NULL,entry TEXT NOT NULL,locator TEXT NOT NULL,PRIMARY KEY(generation,entry));
        CREATE TABLE IF NOT EXISTS catalog_checkpoints(generation TEXT NOT NULL,file TEXT NOT NULL,entry TEXT NOT NULL,PRIMARY KEY(generation,file));
        CREATE TABLE IF NOT EXISTS catalog_cwds(generation TEXT NOT NULL,entry TEXT NOT NULL,cwd_key TEXT NOT NULL,path TEXT NOT NULL,PRIMARY KEY(generation,entry));
        CREATE INDEX IF NOT EXISTS catalog_cwd_keys ON catalog_cwds(generation,cwd_key);
        DELETE FROM catalog_cwds WHERE generation NOT IN(SELECT generation FROM catalog_sources);DELETE FROM catalog_locators WHERE generation NOT IN(SELECT generation FROM catalog_sources);DELETE FROM catalog_checkpoints WHERE generation NOT IN(SELECT generation FROM catalog_sources);
        DELETE FROM catalog_records WHERE generation NOT IN(SELECT generation FROM catalog_sources);
        DELETE FROM catalog_entries WHERE generation NOT IN(SELECT generation FROM catalog_sources);
        PRAGMA user_version=2;");
    if let Err(error) = initialize {
        if matches!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
        ) {
            drop(db);
            directory.verify_location().map_err(|_| "unsafe_cache")?;
            directory
                .remove("catalog.sqlite")
                .map_err(|_| "cache_unavailable")?;
            directory.sync().map_err(|_| "cache_unavailable")?;
            return Err("cache_rebuilding");
        }
        return Err("cache_invalid");
    }
    Ok((directory, lock, db))
}
struct Job {
    scan: Scan,
    generation: String,
    count: u64,
    bytes: usize,
    partial: bool,
    checkpoint: Option<String>,
    backfill_after: String,
}
fn index_worker(shared: &Shared) {
    let mut cache = None;
    let mut last = Instant::now() - Duration::from_secs(60);
    let mut previous = String::new();
    let mut jobs = std::collections::VecDeque::<Job>::new();
    let mut pending = std::collections::VecDeque::<Job>::new();
    while !shared.stop.load(Ordering::Relaxed) {
        let (policy, sources) = match shared.policy() {
            Ok(v) => v,
            Err(code) => {
                shared.status.write().unwrap().clear();
                shared.set(SourceStatus {
                    id: "configuration".into(),
                    kind: "configuration".into(),
                    state: "unavailable".into(),
                    message: "历史目录已暂停，请检查设置中的修复提示。".into(),
                    indexed_entries: 0,
                    identity: String::new(),
                    revision: None,
                    error: Some(code.into()),
                });
                jobs.clear();
                pending.clear();
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let signature = digest(&serde_json::to_string(&sources).unwrap());
        if cache.is_none() {
            match open_cache(shared) {
                Ok(db) => cache = Some(db),
                Err(code) => {
                    let published = (code == "cache_read_only")
                        .then(|| read_db(shared).ok().flatten())
                        .flatten();
                    for source in &sources {
                        let cached = published
                            .as_ref()
                            .and_then(|db| published_source(db, source));
                        shared.set(status(
                            source,
                            if code == "cache_read_only" {
                                "read_only"
                            } else {
                                "unavailable"
                            },
                            if code == "cache_read_only" {
                                "其它实例正在更新目录；本页读取已保存索引。"
                            } else {
                                "历史索引暂不可用；原始记录保留，项目运行不受影响。"
                            },
                            cached.as_ref().map_or(0, |(_, count)| *count),
                            cached.map(|(revision, _)| revision),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
            }
        }
        let (directory, _, db) = cache.as_mut().unwrap();
        if directory.verify_location().is_err() {
            cache = None;
            jobs.clear();
            pending.clear();
            continue;
        }
        let sources_changed = signature != previous;
        let idle = jobs.is_empty() && pending.is_empty();
        // Consume an already queued refresh when starting a changed-source scan
        // too, so startup does not immediately schedule the same work again.
        let refresh_requested =
            (sources_changed || idle) && shared.refresh.swap(false, Ordering::Relaxed);
        if sources_changed
            || (idle && (refresh_requested || last.elapsed() > Duration::from_secs(30)))
        {
            jobs.clear();
            pending.clear();
            // Incomplete staging generations are never visible. Checkpoints remain committed.
            let _=db.execute_batch("DELETE FROM catalog_cwds WHERE generation NOT IN(SELECT generation FROM catalog_sources);DELETE FROM catalog_locators WHERE generation NOT IN(SELECT generation FROM catalog_sources);DELETE FROM catalog_checkpoints WHERE generation NOT IN(SELECT generation FROM catalog_sources);DELETE FROM catalog_records WHERE generation NOT IN(SELECT generation FROM catalog_sources);DELETE FROM catalog_entries WHERE generation NOT IN(SELECT generation FROM catalog_sources);");
            shared
                .status
                .write()
                .unwrap()
                .retain(|s| sources.iter().any(|v| v.id() == s.id));
            let mut keep = std::collections::HashSet::new();
            for source in &sources {
                keep.insert(source.id().to_string());
            }
            let old: Vec<String> = db
                .prepare("SELECT id FROM catalog_sources")
                .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
                .unwrap_or_default();
            for id in old {
                if !keep.contains(&id) {
                    let _ = db.execute("DELETE FROM catalog_sources WHERE id=?", [id]);
                }
            }
            for source in &sources {
                let revision = published_source(db, source).map(|(revision, _)| revision);
                shared.set(status(
                    source,
                    "indexing",
                    "正在重新整理历史目录；原始记录保留。",
                    0,
                    revision,
                ));
                let scan = Scan::open(source.clone());
                #[cfg(test)]
                pressure_tests::record_scan_attempt(source, scan.as_ref().err().copied());
                match scan {
                    Ok(scan) => jobs.push_back(Job {
                        scan,
                        generation: uuid::Uuid::new_v4().to_string(),
                        count: 0,
                        bytes: 0,
                        partial: false,
                        checkpoint: None,
                        backfill_after: String::new(),
                    }),
                    Err(code) => shared.set(source_error(source, code, 0)),
                }
            }
            previous = signature;
            last = Instant::now();
        }
        let Some(mut job) = jobs.pop_front() else {
            if let Some(mut job) = pending.pop_front() {
                let allowance = (policy.cache_limit_mi_b.min(1024) as usize * 1024 * 1024)
                    / (sources.len().max(1) * 4);
                match backfill_workbench(db, &sources, &mut job, allowance) {
                    Ok(true) => publish(db, shared, job),
                    Ok(false) => pending.push_front(job),
                    Err(code) => shared.set(source_error(&job.scan.source, code, job.count)),
                }
                if pending.is_empty() {
                    last = Instant::now();
                }
                std::thread::yield_now();
                continue;
            }
            let _ = db.execute_batch("PRAGMA incremental_vacuum(64);");
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        let allowance =
            (policy.cache_limit_mi_b.min(1024) as usize * 1024 * 1024) / (sources.len().max(1) * 4);
        match job.scan.step(&shared.key) {
            Ok(Step::More) => jobs.push_back(job),
            Ok(Step::Checkpoint(checkpoint)) => {
                match reuse_checkpoint(
                    db,
                    &job.generation,
                    &job.scan.source,
                    &checkpoint,
                    allowance.saturating_sub(job.bytes),
                    (allowance / 2).saturating_sub(job.bytes),
                ) {
                    Ok(Some((bytes, partial, budget_limited))) => {
                        job.partial |= budget_limited;
                        if partial {
                            job.scan.issue("entry_coverage_partial");
                        }
                        job.bytes += bytes;
                        job.count += 1;
                        job.scan.skip_native();
                        shared.set(status(
                            &job.scan.source,
                            "indexing",
                            "正在重新整理历史目录；原始记录保留。",
                            job.count,
                            None,
                        ));
                    }
                    _ => job.checkpoint = Some(checkpoint),
                }
                jobs.push_back(job);
            }
            Ok(Step::Document(mut doc)) => {
                doc.entry.title = safe_text(&doc.entry.title);
                // Reserve half the source allowance for later metadata/locators.
                // Body/search caches must not stop discovery of small entries.
                // Skip redaction when no body can pass admission even at size zero.
                let body_allowance = allowance / 2;
                if job.bytes.saturating_add(64 * 1024) > body_allowance {
                    doc.records.clear();
                    doc.entry.issue("cache_budget");
                    job.partial = true;
                }
                for record in &mut doc.records {
                    *record = sanitize(record, &shared.key);
                }
                if doc.entry.coverage.state == "partial"
                    || doc
                        .records
                        .iter()
                        .map(|r| r.to_string().len())
                        .sum::<usize>()
                        > 64 * 1024
                {
                    job.scan.issue("entry_coverage_partial");
                }
                // Every record is independently pageable. Oversized details are
                // explicitly omitted; metadata never claims they were cached.
                for record in &mut doc.records {
                    if record.to_string().len() > 64 * 1024 {
                        *record = json!({"kind":record["kind"],"detailsOmitted":true,"reason":"detail_limit"});
                        doc.entry.issue("detail_limit");
                    }
                }
                if job.bytes
                    + doc
                        .records
                        .iter()
                        .map(|v| v.to_string().len())
                        .sum::<usize>()
                    + 64 * 1024
                    > body_allowance
                {
                    doc.records.clear();
                    doc.entry.issue("cache_budget");
                    job.partial = true;
                }
                // Metadata also consumes the budget; stop discovery explicitly
                // rather than grow an unbounded catalog after bodies are full.
                if job.bytes + serde_json::to_vec(&doc.entry).unwrap().len() > allowance {
                    job.partial = true;
                    finish_scan(db, shared, job, &mut pending);
                    if jobs.is_empty() && pending.is_empty() {
                        last = Instant::now();
                    }
                    continue;
                }
                let result = write_document(
                    db,
                    &job.generation,
                    &doc,
                    job.checkpoint.take().as_deref(),
                    allowance.saturating_sub(job.bytes),
                );
                match result {
                    Ok(bytes) => {
                        job.bytes += bytes;
                        job.count += 1;
                        shared.set(status(
                            &job.scan.source,
                            "indexing",
                            "正在重新整理历史目录；原始记录保留。",
                            job.count,
                            None,
                        ));
                        jobs.push_back(job);
                    }
                    Err("cache_budget") => {
                        job.partial = true;
                        finish_scan(db, shared, job, &mut pending);
                    }
                    Err(code) => shared.set(status(
                        &job.scan.source,
                        "unavailable",
                        code,
                        job.count,
                        None,
                    )),
                }
            }
            Ok(Step::Done) => finish_scan(db, shared, job, &mut pending),
            Err(code) => shared.set(source_error(&job.scan.source, code, job.count)),
        }
        // A slow scan must still rest for the interval after completion. Measuring
        // only from its start makes long scans run continuously and hides readiness.
        if jobs.is_empty() && pending.is_empty() {
            last = Instant::now();
        }
        std::thread::yield_now();
    }
}
fn published_source(db: &Connection, source: &Source) -> Option<(String, u64)> {
    db.query_row("SELECT revision,(SELECT count(*) FROM catalog_entries e WHERE e.generation=s.generation) FROM catalog_sources s WHERE id=?1 AND identity=?2", params![source.id(),source.identity()], |row| Ok((row.get(0)?,row.get(1)?))).optional().ok().flatten()
}
// Stage workbench sources until native sources have published. This makes
// old workspace-hash recovery independent of discovery order without mutating
// an already visible generation or blocking queries on source I/O.
fn finish_scan(
    db: &mut Connection,
    shared: &Shared,
    job: Job,
    pending: &mut std::collections::VecDeque<Job>,
) {
    if matches!(job.scan.source, Source::Workbench { .. }) {
        pending.push_back(job);
    } else {
        publish(db, shared, job);
    }
}
fn backfill_workbench(
    db: &mut Connection,
    sources: &[Source],
    job: &mut Job,
    allowance: usize,
) -> Result<bool> {
    let generation = &job.generation;
    let after = &mut job.backfill_after;
    let identities = serde_json::to_string(
        &sources
            .iter()
            .filter(|s| matches!(s, Source::Native { .. }))
            .map(Source::identity)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let rows: Vec<(String, String)> = db.prepare("SELECT id,metadata FROM catalog_entries WHERE generation=?1 AND id>?2 AND json_extract(metadata,'$.recordedCwd') IS NULL AND json_extract(metadata,'$.workspaceId') IS NOT NULL ORDER BY id LIMIT 32")
        .and_then(|mut stmt| stmt.query_map(params![generation, after.as_str()], |r| Ok((r.get(0)?,r.get(1)?)))?.collect()).map_err(|_| "cache_unavailable")?;
    let done = rows.len() < 32;
    let tx = db.transaction().map_err(|_| "cache_busy")?;
    for (id, metadata) in rows {
        *after = id.clone();
        let mut entry: Entry = serde_json::from_str(&metadata).map_err(|_| "cache_invalid")?;
        let paths: Vec<String> = tx.prepare("SELECT DISTINCT c.path FROM catalog_cwds c JOIN catalog_sources s ON s.generation=c.generation WHERE c.cwd_key=?1 AND s.identity IN(SELECT value FROM json_each(?2)) LIMIT 2")
            .and_then(|mut stmt| stmt.query_map(params![entry.workspace_id, identities], |r| r.get(0))?.collect()).map_err(|_| "cache_unavailable")?;
        if paths.len() == 1 {
            entry.path(&paths[0]);
            let updated = serde_json::to_string(&entry).unwrap();
            let extra = updated.len().saturating_sub(metadata.len());
            if job.bytes.saturating_add(extra) > allowance {
                job.partial = true;
                continue;
            }
            job.bytes += extra;
            tx.execute(
                "UPDATE catalog_entries SET project=?1,metadata=?2 WHERE generation=?3 AND id=?4",
                params![entry.project_id, updated, generation, id],
            )
            .map_err(|_| "cache_write_failed")?;
        }
    }
    tx.commit().map_err(|_| "cache_write_failed")?;
    Ok(done)
}
fn publish(db: &mut Connection, shared: &Shared, mut job: Job) {
    if job.partial {
        job.scan.issues.push("cache_budget".into());
    }
    let source = &job.scan.source;
    if !shared
        .policy()
        .is_ok_and(|(_, sources)| sources.contains(source))
    {
        return;
    }
    // An unchanged rescan must not invalidate a reader's pagination.
    let revision = (|| -> rusqlite::Result<String> {
        let mut hash = blake3::Hasher::new();
        hash.update(&CATALOG_VERSION.to_le_bytes());
        hash.update(source.identity().as_bytes());
        hash.update(serde_json::to_string(&job.scan.issues).unwrap().as_bytes());
        let mut statement = db.prepare(
            "SELECT metadata,search FROM catalog_entries WHERE generation=? ORDER BY id",
        )?;
        for row in statement.query_map([&job.generation], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (metadata, search) = row?;
            hash.update(metadata.as_bytes());
            hash.update(search.as_bytes());
        }
        Ok(hash.finalize().to_hex().to_string())
    })();
    let Ok(revision) = revision else { return };
    let result = db.execute(
        "INSERT OR REPLACE INTO catalog_sources VALUES (?1,?2,?3,?4,?5)",
        params![
            source.id(),
            source.identity(),
            job.generation,
            revision,
            serde_json::to_string(&job.scan.issues).unwrap()
        ],
    );
    if result.is_ok() {
        let count = db
            .query_row(
                "SELECT count(*) FROM catalog_entries WHERE generation=?",
                [&job.generation],
                |r| r.get::<_, u64>(0),
            )
            .unwrap_or(job.count);
        shared.set(status(
            source,
            if job.scan.issues.is_empty() {
                "ready"
            } else {
                "partial"
            },
            &format!(
                "已索引 {count} 条记录{}",
                if job.scan.issues.is_empty() {
                    ""
                } else {
                    "；部分内容未能读取，搜索仅覆盖已索引内容。"
                }
            ),
            count,
            Some(revision),
        ));
    } else {
        shared.set(status(
            source,
            "unavailable",
            "索引保存失败；原始记录未修改。",
            job.count,
            None,
        ));
    }
}
fn write_document(
    db: &mut Connection,
    generation: &str,
    document: &Document,
    checkpoint: Option<&str>,
    budget: usize,
) -> Result<usize> {
    let entry = &document.entry;
    let tx = db.transaction().map_err(|_| "cache_busy")?;
    // A native file moved to archives retains the same entry ID. Replace, never append duplicate bodies.
    tx.execute(
        "DELETE FROM catalog_records WHERE generation=? AND entry=?",
        params![generation, entry.entry_id],
    )
    .map_err(|_| "cache_write_failed")?;
    tx.execute(
        "INSERT OR REPLACE INTO catalog_locators VALUES (?1,?2,?3)",
        params![generation, entry.entry_id, document.locator.to_string()],
    )
    .map_err(|_| "cache_write_failed")?;
    let mut cwd_bytes = 0;
    if entry.kind == "native" {
        tx.execute(
            "DELETE FROM catalog_cwds WHERE generation=?1 AND entry=?2",
            params![generation, entry.entry_id],
        )
        .map_err(|_| "cache_write_failed")?;
        if let Some((key, path)) = entry.recorded_cwd.as_deref().and_then(project) {
            cwd_bytes = key.len() + path.len();
            tx.execute(
                "INSERT INTO catalog_cwds VALUES (?1,?2,?3,?4)",
                params![
                    generation,
                    entry.entry_id,
                    key.trim_start_matches("p_"),
                    path
                ],
            )
            .map_err(|_| "cache_write_failed")?;
        }
    }
    let mut search = String::new();
    let mut bytes = 0;
    for (index, record) in document.records.iter().enumerate() {
        let body = record.to_string();
        bytes += body.len();
        if search.len() < 64 * 1024 {
            let remaining = 64 * 1024 - search.len();
            search.extend(body.chars().take(remaining / 4));
        }
        tx.execute(
            "INSERT INTO catalog_records VALUES (?1,?2,?3,?4)",
            params![generation, entry.entry_id, index as i64, body],
        )
        .map_err(|_| "cache_write_failed")?;
    }
    let mut entry = entry.clone();
    if bytes > 64 * 1024 {
        entry.issue("search_excerpt_limit");
    }
    let metadata = serde_json::to_string(&entry).unwrap();
    bytes += metadata.len() + search.len() + cwd_bytes;
    if bytes > budget {
        return Err("cache_budget");
    }
    tx.execute(
        "INSERT OR REPLACE INTO catalog_entries VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            generation,
            entry.entry_id,
            entry.source_id,
            entry.project_id,
            entry.kind,
            entry.recorded_at.as_deref().unwrap_or(""),
            metadata,
            search
        ],
    )
    .map_err(|_| "cache_write_failed")?;
    if let Some(checkpoint) = checkpoint {
        tx.execute(
            "INSERT OR REPLACE INTO catalog_checkpoints VALUES (?1,?2,?3)",
            params![generation, checkpoint, entry.entry_id],
        )
        .map_err(|_| "cache_write_failed")?;
    }
    tx.commit().map_err(|_| "cache_write_failed")?;
    Ok(bytes)
}
fn read_db(shared: &Shared) -> Result<Option<Connection>> {
    let dir = match Directory::read_only(&shared.data.join("library")) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cache_unavailable"),
    };
    match dir.open("catalog.sqlite", false) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("unsafe_cache"),
    }
    let db = Connection::open_with_flags(
        db_path(shared),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "cache_unavailable")?;
    db.busy_timeout(Duration::from_millis(80))
        .map_err(|_| "cache_unavailable")?;
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| "cache_invalid")?;
    if version != CATALOG_VERSION {
        return if (0..CATALOG_VERSION).contains(&version) {
            Ok(None)
        } else {
            Err("cache_version_unsupported")
        };
    }
    db.execute_batch("PRAGMA query_only=ON;PRAGMA cache_size=-4096;")
        .map_err(|_| "cache_unavailable")?;
    let ready: bool=db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='catalog_sources')",[],|r|r.get(0)).map_err(|_|"cache_unavailable")?;
    if !ready {
        return Ok(None);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    db.progress_handler(1000, Some(move || Instant::now() > deadline));
    Ok(Some(db))
}
fn query(shared: &Shared, operation: Operation) -> Result<Value> {
    let (_, sources) = shared.policy()?;
    match operation {
        Operation::Sources => Ok(json!({"sources":shared.states(&sources)})),
        Operation::Preview(path) => Ok(json!({"source":scan::preview(path)?})),
        Operation::Refresh => {
            shared.refresh.store(true, Ordering::Relaxed);
            Ok(json!({"scheduled":true}))
        }
        Operation::Launch(project, entry) => {
            let selected = launch::resolve(shared, &sources, project, entry)?;
            Ok(serde_json::to_value(selected).unwrap())
        }
        Operation::List(query, projects) => {
            catalog_query(shared, &sources, query, None, projects, false)
        }
        Operation::Body(id, query) => {
            catalog_query(shared, &sources, query, Some(id), false, false)
        }
        Operation::Details(id, query) => {
            catalog_query(shared, &sources, query, Some(id), false, true)
        }
    }
}
fn catalog_query(
    shared: &Shared,
    sources: &[Source],
    mut query: Query,
    entry: Option<String>,
    projects: bool,
    details: bool,
) -> Result<Value> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    if query.q.as_ref().is_some_and(|s| s.len() > 512)
        || query.cursor.as_ref().is_some_and(|s| s.len() > 4096)
        || query.limit.is_some_and(|v| v == 0 || v > 200)
        || query
            .kind
            .as_ref()
            .is_some_and(|s| !matches!(s.as_str(), "native" | "workbench" | "observer"))
    {
        return Err("invalid_query");
    }
    if query.window {
        let result = window::read(
            shared,
            sources,
            query,
            entry.ok_or("invalid_query")?,
            details,
        );
        if matches!(
            result,
            Err("source_revision_changed" | "search_position_unavailable")
        ) {
            shared.refresh.store(true, Ordering::Relaxed);
        }
        return result;
    }
    if query.source_id.as_ref().is_some_and(|s| s.len() > 64)
        || query.record.is_some()
        || query
            .group
            .as_ref()
            .is_some_and(|s| !matches!(s.as_str(), "main" | "agents"))
    {
        return Err("invalid_query");
    }
    if query.project_id.as_ref().is_some_and(|v| v.len() > 128)
        || query
            .source_revision
            .as_ref()
            .is_some_and(|v| v.len() > 256)
        || (entry.is_none() && query.source_revision.is_some())
    {
        return Err("invalid_query");
    }
    let cursor = query.cursor.take();
    let limit = query.limit.unwrap_or(50);
    query.limit = Some(limit);
    let binding = digest(&json!([query, entry, projects, details]).to_string());
    let Some(db) = read_db(shared)? else {
        return Ok(json!(Page {
            revision: "pending".into(),
            records: vec![],
            next_cursor: None,
            coverage: Coverage {
                state: "partial".into(),
                reasons: vec!["indexing".into()]
            }
        }));
    };
    // One read transaction pins source generations and all query rows together.
    db.execute_batch("BEGIN DEFERRED")
        .map_err(|_| "cache_busy")?;
    let mut generation = vec![];
    let mut revisions = vec![];
    let mut reasons = shared
        .states(sources)
        .iter()
        .filter(|s| matches!(s.state.as_str(), "unavailable" | "indexing" | "partial"))
        .map(|s| format!("source_{}:{}", s.state, s.id))
        .collect::<Vec<_>>();
    for source in sources
        .iter()
        .filter(|s| query.source_id.as_ref().is_none_or(|id| id == s.id()))
    {
        let row:Option<(String,String,String)>=db.query_row("SELECT generation,revision,issues FROM catalog_sources WHERE id=?1 AND identity=?2",params![source.id(),source.identity()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|_|"cache_unavailable")?;
        if let Some((generation_id, revision, issues)) = row {
            generation.push(generation_id);
            revisions.push(revision);
            reasons.extend(
                serde_json::from_str::<Vec<String>>(&issues)
                    .unwrap_or_else(|_| vec!["cache_invalid".into()]),
            );
        } else {
            reasons.push("indexing".into());
        }
    }
    let revision = digest(&json!([CATALOG_VERSION, revisions, sources]).to_string());
    let offset = if let Some(cursor) = cursor {
        let bytes = URL_SAFE_NO_PAD
            .decode(cursor)
            .map_err(|_| "invalid_cursor")?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid_cursor")?;
        let signature = value["signature"].as_str().ok_or("invalid_cursor")?;
        let payload = json!([value["revision"], value["binding"], value["offset"]]).to_string();
        if signature
            != blake3::keyed_hash(&shared.key, payload.as_bytes())
                .to_hex()
                .as_str()
        {
            return Err("invalid_cursor");
        }
        if value["revision"] != revision || value["binding"] != binding {
            return Err("stale_cursor");
        }
        value["offset"]
            .as_u64()
            .filter(|n| *n <= 10_000_000)
            .ok_or("invalid_cursor")?
    } else {
        0
    };
    let generations = serde_json::to_string(&generation).unwrap();
    let pattern = query.q.as_deref().unwrap_or("");
    let mut metadata = None;
    let rows: Vec<Value> = if let Some(ref id) = entry {
        let found:Option<String>=db.query_row("SELECT metadata FROM catalog_entries WHERE generation IN(SELECT value FROM json_each(?1)) AND id=?2",params![generations,id],|r|r.get(0)).optional().map_err(|_|"cache_unavailable")?;
        let Some(found) = found else {
            return Err("entry_unavailable");
        };
        let entry: Entry = serde_json::from_str(&found).map_err(|_| "cache_invalid")?;
        if query
            .source_revision
            .as_ref()
            .is_some_and(|revision| revision != &entry.source_revision)
        {
            return Err("source_revision_changed");
        }
        let mut entry_value = serde_json::to_value(&entry).unwrap();
        relate(&db, sources, &generations, &mut entry_value)?;
        metadata = Some(entry_value);
        if details && !entry.capabilities.inspect_calls {
            return Err("details_unavailable");
        }
        reasons = entry.coverage.reasons;
        if shared
            .states(sources)
            .iter()
            .any(|s| s.id == entry.source_id && s.state == "unavailable")
        {
            reasons.push("source_unavailable_cached_copy".into());
        }
        let sql = if details {
            "SELECT body FROM catalog_records WHERE generation IN(SELECT value FROM json_each(?1)) AND entry=?2 AND json_extract(body,'$.kind') IN ('call_detail','observer_instructions') ORDER BY seq LIMIT ?3 OFFSET ?4"
        } else {
            "SELECT body FROM catalog_records WHERE generation IN(SELECT value FROM json_each(?1)) AND entry=?2 ORDER BY seq LIMIT ?3 OFFSET ?4"
        };
        let mut stmt = db.prepare(sql).map_err(|_| "cache_unavailable")?;
        stmt.query_map(params![generations, id, limit + 1, offset], |r| {
            r.get::<_, String>(0)
        })
        .map_err(|_| "cache_unavailable")?
        .map(|v| {
            serde_json::from_str(&v.map_err(|_| "cache_unavailable")?).map_err(|_| "cache_invalid")
        })
        .collect::<Result<_>>()?
    } else {
        let sql = if projects {
            "SELECT json_object('projectId',project,'path',min(json_extract(metadata,'$.projectPath')),'latestRecordedAt',nullif(max(time),''),'entries',count(*)) FROM catalog_entries WHERE project IS NOT NULL AND generation IN(SELECT value FROM json_each(?1)) AND (?2 IS NULL OR project=?2 OR (?2='unassigned' AND project IS NULL)) AND (?3 IS NULL OR kind=?3) AND (?4='' OR instr(lower(metadata||search),lower(?4))>0) AND (?7 IS NULL OR (?7='agents' AND (json_extract(metadata,'$.isSubagent')=1 OR json_extract(metadata,'$.parentThreadId') IS NOT NULL)) OR (?7='main' AND coalesce(json_extract(metadata,'$.isSubagent'),0)=0 AND json_extract(metadata,'$.parentThreadId') IS NULL)) GROUP BY project ORDER BY max(time) DESC,min(json_extract(metadata,'$.projectPath')),project LIMIT ?5 OFFSET ?6"
        } else {
            "SELECT metadata FROM catalog_entries WHERE generation IN(SELECT value FROM json_each(?1)) AND (?2 IS NULL OR project=?2 OR (?2='unassigned' AND project IS NULL)) AND (?3 IS NULL OR kind=?3) AND (?4='' OR instr(lower(metadata||search),lower(?4))>0) AND (?7 IS NULL OR (?7='agents' AND (json_extract(metadata,'$.isSubagent')=1 OR json_extract(metadata,'$.parentThreadId') IS NOT NULL)) OR (?7='main' AND coalesce(json_extract(metadata,'$.isSubagent'),0)=0 AND json_extract(metadata,'$.parentThreadId') IS NULL)) ORDER BY time DESC,id LIMIT ?5 OFFSET ?6"
        };
        let mut stmt = db.prepare(sql).map_err(|_| "cache_unavailable")?;
        stmt.query_map(
            params![
                generations,
                query.project_id,
                query.kind,
                pattern,
                limit + 1,
                offset,
                query.group
            ],
            |r| r.get::<_, String>(0),
        )
        .map_err(|_| "cache_unavailable")?
        .map(|v| {
            serde_json::from_str(&v.map_err(|_| "cache_unavailable")?).map_err(|_| "cache_invalid")
        })
        .collect::<Result<_>>()?
    };
    let total = rows.len();
    let mut records = vec![];
    let mut bytes = 0;
    for mut row in rows.into_iter().take(limit) {
        if entry.is_none() && !projects {
            relate(&db, sources, &generations, &mut row)?;
            if !pattern.is_empty() {
                let hit:Option<(u64,String)> = db.query_row("SELECT seq,body FROM catalog_records WHERE generation IN(SELECT value FROM json_each(?1)) AND entry=?2 AND instr(lower(body),lower(?3))>0 ORDER BY seq LIMIT 1",params![generations,row["entryId"].as_str(),pattern],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|"cache_unavailable")?;
                if let Some((seq, body)) = hit {
                    let record: Value = serde_json::from_str(&body).map_err(|_| "cache_invalid")?;
                    row["match"] = json!({"record":seq,"offset":record["offset"],"sourceRevision":row["sourceRevision"],"text":record["text"].as_str().or(record["raw"]["summaryText"].as_str()).unwrap_or("上下文或详情中包含搜索内容").chars().take(200).collect::<String>()});
                }
            }
        }
        let size = row.to_string().len();
        if size > 900 * 1024 {
            return Err("record_too_large");
        }
        if bytes + size > 900 * 1024 {
            break;
        }
        bytes += size;
        records.push(row);
    }
    let next = offset + records.len() as u64;
    let next_cursor = if records.len() < total {
        let payload = json!([revision, binding, next]).to_string();
        Some(URL_SAFE_NO_PAD.encode(json!({"revision":revision,"binding":binding,"offset":next,"signature":blake3::keyed_hash(&shared.key,payload.as_bytes()).to_hex().to_string()}).to_string()))
    } else {
        None
    };
    reasons.sort();
    reasons.dedup();
    let mut result = json!(Page {
        revision,
        records,
        next_cursor,
        coverage: Coverage {
            state: if reasons.is_empty() {
                "complete_for_source"
            } else {
                "partial"
            }
            .into(),
            reasons
        }
    });
    if entry.is_none() && projects {
        let count: u64 = db.query_row("SELECT count(*) FROM catalog_entries WHERE generation IN(SELECT value FROM json_each(?1)) AND project IS NULL AND (?2 IS NULL OR kind=?2) AND (?3='' OR instr(lower(metadata||search),lower(?3))>0) AND (?4 IS NULL OR (?4='agents' AND (json_extract(metadata,'$.isSubagent')=1 OR json_extract(metadata,'$.parentThreadId') IS NOT NULL)) OR (?4='main' AND coalesce(json_extract(metadata,'$.isSubagent'),0)=0 AND json_extract(metadata,'$.parentThreadId') IS NULL))",params![generations,query.kind,pattern,query.group],|r|r.get(0)).map_err(|_| "cache_unavailable")?;
        result["unassignedRecords"] = json!(count);
    }
    if entry.is_none()
        && !projects
        && let Some(project) = query.project_id.as_deref().filter(|p| *p != "unassigned")
    {
        let identities =
            serde_json::to_string(&sources.iter().map(Source::identity).collect::<Vec<_>>())
                .unwrap();
        let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_entries e JOIN catalog_sources s ON s.generation=e.generation WHERE e.project=?1 AND s.identity IN(SELECT value FROM json_each(?2)))",params![project,identities],|r|r.get(0)).map_err(|_| "cache_unavailable")?;
        let published: usize = db.query_row("SELECT count(*) FROM catalog_sources WHERE identity IN(SELECT value FROM json_each(?1))",[&identities],|r|r.get(0)).map_err(|_| "cache_unavailable")?;
        let incomplete: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_sources s, json_each(s.issues) issue WHERE s.identity IN(SELECT value FROM json_each(?1)) AND issue.value != 'entry_coverage_partial')",[&identities],|r|r.get(0)).map_err(|_| "cache_unavailable")?;
        if exists
            || (!incomplete
                && published == sources.len()
                && shared
                    .states(sources)
                    .iter()
                    .all(|s| matches!(s.state.as_str(), "ready" | "partial" | "read_only")))
        {
            result["projectExists"] = json!(exists);
        }
    }
    if let Some(metadata) = metadata {
        result["entry"] = metadata;
    }
    if shared.policy()?.1 != sources {
        return Err("source_revoked");
    }
    Ok(result)
}

fn reuse_checkpoint(
    db: &mut Connection,
    generation: &str,
    source: &Source,
    file: &str,
    budget: usize,
    body_budget: usize,
) -> Result<Option<(usize, bool, bool)>> {
    let tx = db.transaction().map_err(|_| "cache_busy")?;
    let previous: Option<(String, String, String, usize, usize)> = tx.query_row(
        "SELECT s.generation,c.entry,e.metadata,
         coalesce((SELECT length(CAST(path AS BLOB))+length(cwd_key)+2 FROM catalog_cwds cwd WHERE cwd.generation=e.generation AND cwd.entry=e.id),0),
         length(CAST(e.search AS BLOB))+coalesce((SELECT sum(length(CAST(body AS BLOB))) FROM catalog_records r WHERE r.generation=s.generation AND r.entry=c.entry),0)
         FROM catalog_sources s JOIN catalog_checkpoints c ON c.generation=s.generation JOIN catalog_entries e ON e.generation=c.generation AND e.id=c.entry
         WHERE s.id=?1 AND s.identity=?2 AND c.file=?3 AND json_extract(e.metadata,'$.sourceIdentity')=?2
         AND EXISTS(SELECT 1 FROM catalog_locators l WHERE l.generation=e.generation AND l.entry=e.id)",
        params![source.id(),source.identity(),file],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    ).optional().map_err(|_| "cache_invalid")?;
    let Some((old, entry, metadata, cwd_bytes, body_bytes)) = previous else {
        return Ok(None);
    };
    let mut metadata_entry: Entry = serde_json::from_str(&metadata).map_err(|_| "cache_invalid")?;
    let mut bytes = metadata.len() + cwd_bytes + body_bytes;
    let omit_body = body_bytes > 0 && bytes > body_budget;
    if omit_body {
        metadata_entry.issue("cache_budget");
        bytes = serde_json::to_string(&metadata_entry).unwrap().len() + cwd_bytes;
    }
    if bytes > budget {
        return Ok(None);
    }
    tx.execute("INSERT OR REPLACE INTO catalog_entries SELECT ?1,id,source,project,kind,time,metadata,search FROM catalog_entries WHERE generation=?2 AND id=?3",params![generation,old,entry]).map_err(|_|"cache_write_failed")?;
    tx.execute("INSERT OR REPLACE INTO catalog_cwds SELECT ?1,entry,cwd_key,path FROM catalog_cwds WHERE generation=?2 AND entry=?3",params![generation,old,entry]).map_err(|_|"cache_write_failed")?;
    if omit_body {
        tx.execute(
            "UPDATE catalog_entries SET metadata=?1,search='' WHERE generation=?2 AND id=?3",
            params![
                serde_json::to_string(&metadata_entry).unwrap(),
                generation,
                entry
            ],
        )
        .map_err(|_| "cache_write_failed")?;
        tx.execute(
            "DELETE FROM catalog_records WHERE generation=?1 AND entry=?2",
            params![generation, entry],
        )
        .map_err(|_| "cache_write_failed")?;
    } else {
        tx.execute("INSERT OR REPLACE INTO catalog_records SELECT ?1,entry,seq,body FROM catalog_records WHERE generation=?2 AND entry=?3",params![generation,old,entry]).map_err(|_|"cache_write_failed")?;
    }
    tx.execute("INSERT OR REPLACE INTO catalog_locators SELECT ?1,entry,locator FROM catalog_locators WHERE generation=?2 AND entry=?3",params![generation,old,entry]).map_err(|_|"cache_write_failed")?;
    tx.execute(
        "INSERT OR REPLACE INTO catalog_checkpoints VALUES (?1,?2,?3)",
        params![generation, file, entry],
    )
    .map_err(|_| "cache_write_failed")?;
    tx.commit().map_err(|_| "cache_write_failed")?;
    Ok(Some((
        bytes,
        metadata_entry.coverage.state != "complete_for_source",
        metadata_entry
            .coverage
            .reasons
            .iter()
            .any(|r| r == "cache_budget"),
    )))
}

fn relate(db: &Connection, sources: &[Source], generations: &str, entry: &mut Value) -> Result<()> {
    if let Some(parent) = entry["parentEntryId"].as_str() {
        let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_entries WHERE generation IN(SELECT value FROM json_each(?1)) AND id=?2 AND source=?3)",params![generations,parent,entry["sourceId"].as_str()],|r|r.get(0)).map_err(|_| "cache_unavailable")?;
        if !exists {
            entry["parentEntryId"] = Value::Null;
        }
    }
    let Some(thread) = entry["nativeThreadId"].as_str() else {
        return Ok(());
    };
    let Some(source) = sources
        .iter()
        .find(|s| Some(s.id()) == entry["sourceId"].as_str())
    else {
        return Ok(());
    };
    let native_home = match source {
        Source::Native { codex_home, .. } => Some(codex_home),
        Source::Observer { native_home, .. } => native_home.as_ref(),
        _ => None,
    };
    let Some(home) = native_home else {
        return Ok(());
    };
    let canonical = home.canonicalize().unwrap_or_else(|_| home.clone());
    let ids: Vec<_> = sources
        .iter()
        .filter(|other| other.id() != source.id())
        .filter_map(|other| {
            let other_home = match other {
                Source::Native { codex_home, .. } => Some(codex_home),
                Source::Observer { native_home, .. } => native_home.as_ref(),
                _ => None,
            }?;
            (canonical
                == other_home
                    .canonicalize()
                    .unwrap_or_else(|_| other_home.clone()))
            .then_some(other.id())
        })
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let mut stmt=db.prepare("SELECT id FROM catalog_entries WHERE generation IN(SELECT value FROM json_each(?1)) AND source IN(SELECT value FROM json_each(?2)) AND json_extract(metadata,'$.nativeThreadId')=?3 ORDER BY id LIMIT 16").map_err(|_|"cache_unavailable")?;
    let related = stmt
        .query_map(
            params![generations, serde_json::to_string(&ids).unwrap(), thread],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| "cache_unavailable")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| "cache_unavailable")?;
    entry["relatedEntryIds"] = json!(related);
    Ok(())
}

fn source_error(source: &Source, code: &str, count: u64) -> SourceStatus {
    let mut result = status(
        source,
        "unavailable",
        if code == "source_unavailable" {
            "无法读取历史位置，请检查目录是否存在以及读取权限。"
        } else {
            "历史更新未完成，请检查来源文件或稍后重试；已有记录保留。"
        },
        count,
        None,
    );
    result.error = Some(code.into());
    result
}

#[cfg(test)]
mod scale_tests;

#[cfg(test)]
mod fault_tests;

#[cfg(test)]
mod pressure_tests;
