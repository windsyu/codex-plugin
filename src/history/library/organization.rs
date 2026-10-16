//! User organization is durable intent, never a rebuildable catalog.
use super::Result;
use crate::workbench::recording::fs::{Directory, Identity};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::File,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

const ROWS: usize = 1024;
const BYTES: usize = 8 * 1024 * 1024;
const FIELD: usize = 4096;
const FILE_BYTES: u64 = 32 * 1024 * 1024;
const BUSY: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Override {
    pub source_identity: String,
    pub identity_digest: String,
    pub disposition: String,
    pub project_id: Option<String>,
    pub project_path: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub revision: String,
    pub overrides: BTreeMap<String, Override>,
}
pub(super) struct Store {
    data: PathBuf,
    seen: Mutex<Option<Seen>>,
    #[cfg(test)]
    fault: Mutex<Option<Fault>>,
}
#[derive(Clone, PartialEq, Eq)]
struct Seen {
    root: Identity,
    directory: Identity,
    file: Identity,
    store_id: String,
}
#[cfg(test)]
#[derive(Clone, Copy)]
enum Fault {
    BeforeCommitCrash,
    AfterCommitCrash,
    Full,
}
struct Files {
    root: Directory,
    directory: Directory,
    file: File,
    name: String,
}
impl Store {
    pub fn new(data: PathBuf) -> Self {
        Self {
            data,
            seen: Mutex::new(None),
            #[cfg(test)]
            fault: Mutex::new(None),
        }
    }
    pub fn snapshot(&self) -> Result<Snapshot> {
        let mut seen = self.seen.lock().map_err(|_| "organization_unavailable")?;
        let Some(files) = self.existing(&seen)? else {
            return Ok(absent());
        };
        let lock = files
            .directory
            .open("bootstrap.lock", false)
            .map_err(fs_error)?;
        private_file(&lock)?;
        let _guard = Bootstrap::acquire(lock)?;
        // Only our verified state database may perform native hot-journal
        // recovery. This connection cannot CREATE a missing main database.
        let mut db = connect(&files, true)?;
        db.execute_batch("PRAGMA query_only=ON;")
            .map_err(|_| "organization_unavailable")?;
        let tx = db.transaction().map_err(|_| "organization_unavailable")?;
        let (snapshot, store_id) = load(&tx)?;
        let observed = observation(&files, store_id)?;
        check_seen(&seen, &observed)?;
        verify(&files)?;
        tx.commit().map_err(|_| "organization_unavailable")?;
        *seen = Some(observed);
        Ok(snapshot)
    }
    pub fn save(
        &self,
        expected: &str,
        entry_id: &str,
        validate: impl FnOnce(&Snapshot) -> Result<Option<Override>>,
    ) -> Result<Snapshot> {
        if entry_id.is_empty() || entry_id.len() > FIELD || entry_id.contains('\0') {
            return Err("organization_unavailable");
        }
        let mut seen = self.seen.lock().map_err(|_| "organization_unavailable")?;
        // A disappearing observed database must never be recreated by a write.
        if seen.is_some() {
            self.existing(&seen)?.ok_or("organization_unavailable")?;
        }
        if expected != "absent" && self.existing(&seen)?.is_none() {
            return Err("organization_changed");
        }
        let root = Directory::root(&self.data).map_err(fs_error)?;
        private_dir(&root)?;
        let directory = root.dir("organization", true).map_err(fs_error)?;
        private_dir(&directory)?;
        sidecars(&directory)?;
        let lock = directory
            .open("bootstrap.lock", true)
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    directory.open("bootstrap.lock", false)
                } else {
                    Err(e)
                }
            })
            .map_err(fs_error)?;
        private_file(&lock)?;
        let _guard = Bootstrap::acquire(lock)?;
        root.verify_location().map_err(fs_error)?;
        directory.verify_location().map_err(fs_error)?;
        sidecars(&directory)?;
        let (file, created, name) = match directory.open("state.sqlite", false) {
            Ok(file) => (file, false, "state.sqlite".into()),
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && seen.is_none()
                    && expected == "absent" =>
            {
                let name = format!("pending-{}.sqlite", uuid::Uuid::new_v4());
                (directory.open(&name, true).map_err(fs_error)?, true, name)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err("organization_unavailable");
            }
            Err(e) => return Err(fs_error(e)),
        };
        private_file(&file)?;
        if !created && file.metadata().map_err(fs_error)?.len() == 0 {
            return Err("organization_unavailable");
        }
        let mut files = Files {
            root,
            directory,
            file,
            name,
        };
        let mut db = connect(&files, true)?;
        db.execute_batch("PRAGMA synchronous=FULL; PRAGMA temp_store=MEMORY;")
            .map_err(|_| "organization_unavailable")?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "organization_unavailable")?;
        let (before, store_id) = if created {
            (absent(), uuid::Uuid::new_v4().to_string())
        } else {
            load(&tx)?
        };
        if !created {
            let observed = observation(&files, store_id.clone())?;
            check_seen(&seen, &observed)?;
            *seen = Some(observed);
        }
        if expected != before.revision {
            return Err("organization_changed");
        }
        // Validation belongs inside the same writer transaction as the CAS.
        let replacement = validate(&before)?;
        if let Some(row) = &replacement {
            validate_row(row)?;
        }
        let mut after = before.clone();
        match replacement {
            Some(row) => {
                after.overrides.insert(entry_id.into(), row);
            }
            None => {
                after.overrides.remove(entry_id);
            }
        }
        budget(&after.overrides)?;
        let revision = if created {
            1
        } else {
            revision_number(&before.revision)?
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or("organization_unavailable")?
        };
        if created {
            tx.execute_batch("CREATE TABLE organization_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),schema_version INTEGER NOT NULL,store_id TEXT NOT NULL,revision INTEGER NOT NULL); CREATE TABLE project_overrides(entry_id TEXT PRIMARY KEY,source_identity TEXT NOT NULL,identity_digest TEXT NOT NULL,disposition TEXT NOT NULL CHECK(disposition IN ('project','unassigned')),project_id TEXT,project_path TEXT); PRAGMA user_version=1;").map_err(|_| "organization_unavailable")?;
            tx.execute(
                "INSERT INTO organization_meta VALUES(1,1,?1,?2)",
                params![store_id, revision as i64],
            )
            .map_err(|_| "organization_unavailable")?;
        } else {
            tx.execute(
                "UPDATE organization_meta SET revision=?1 WHERE singleton=1",
                [revision as i64],
            )
            .map_err(|_| "organization_unavailable")?;
        }
        #[cfg(test)]
        if matches!(*self.fault.lock().unwrap(), Some(Fault::Full)) {
            let pages: i64 = tx
                .pragma_query_value(None, "page_count", |r| r.get(0))
                .unwrap();
            tx.pragma_update(None, "max_page_count", pages).unwrap();
            let error = tx.execute("INSERT INTO project_overrides VALUES('disk-full',?1,'identity','unassigned',NULL,NULL)", ["x".repeat(1024 * 1024)]).unwrap_err();
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DiskFull)
            );
            return Err("organization_unavailable");
        }
        if let Some(row) = after.overrides.get(entry_id) {
            tx.execute("INSERT INTO project_overrides VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(entry_id) DO UPDATE SET source_identity=excluded.source_identity,identity_digest=excluded.identity_digest,disposition=excluded.disposition,project_id=excluded.project_id,project_path=excluded.project_path", params![entry_id,row.source_identity,row.identity_digest,row.disposition,row.project_id,row.project_path]).map_err(|_| "organization_unavailable")?;
        } else {
            tx.execute(
                "DELETE FROM project_overrides WHERE entry_id=?1",
                [entry_id],
            )
            .map_err(|_| "organization_unavailable")?;
        }
        verify(&files)?;
        #[cfg(test)]
        if matches!(*self.fault.lock().unwrap(), Some(Fault::BeforeCommitCrash)) {
            std::process::exit(55);
        }
        tx.commit().map_err(|_| "organization_unavailable")?;
        if created {
            // An orphaned pending database is never interpreted as published
            // intent. Publication cannot overwrite a concurrent/unknown file.
            match files.directory.entry_info("state.sqlite") {
                Ok(_) => return Err("organization_changed"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(fs_error(e)),
            }
            drop(db);
            files
                .directory
                .publish_file_no_replace(&files.name, "state.sqlite", file_identity(&files.file)?)
                .map_err(fs_error)?;
            files.name = "state.sqlite".into();
        }
        files.directory.sync().map_err(fs_error)?;
        #[cfg(test)]
        if matches!(*self.fault.lock().unwrap(), Some(Fault::AfterCommitCrash)) {
            std::process::exit(56);
        }
        verify(&files)?;
        let observed = observation(&files, store_id.clone())?;
        check_seen(&seen, &observed)?;
        *seen = Some(observed);
        after.revision = format!("{store_id}:{revision}");
        Ok(after)
    }

    fn existing(&self, seen: &Option<Seen>) -> Result<Option<Files>> {
        let root = match Directory::read_only(&self.data) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && seen.is_none() => {
                return Ok(None);
            }
            Err(e) => return Err(fs_error(e)),
        };
        private_dir(&root)?;
        let directory = match root.dir("organization", false) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && seen.is_none() => {
                return Ok(None);
            }
            Err(e) => return Err(fs_error(e)),
        };
        private_dir(&directory)?;
        sidecars(&directory)?;
        let file = match directory.open("state.sqlite", false) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && seen.is_none() => {
                return Ok(None);
            }
            Err(e) => return Err(fs_error(e)),
        };
        private_file(&file)?;
        let files = Files {
            root,
            directory,
            file,
            name: "state.sqlite".into(),
        };
        if files.file.metadata().map_err(fs_error)?.len() == 0 {
            return Err("organization_unavailable");
        }
        verify(&files)?;
        if let Some(old) = seen {
            let identity = observation(&files, old.store_id.clone())?;
            check_seen(seen, &identity)?;
        }
        Ok(Some(files))
    }
}

fn absent() -> Snapshot {
    Snapshot {
        revision: "absent".into(),
        overrides: BTreeMap::new(),
    }
}
fn fs_error(error: std::io::Error) -> &'static str {
    if error.kind() == std::io::ErrorKind::PermissionDenied
        || matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR))
    {
        "unsafe_organization"
    } else {
        "organization_unavailable"
    }
}
fn private_dir(dir: &Directory) -> Result<()> {
    dir.verify_location().map_err(fs_error)?;
    let m = std::fs::symlink_metadata(&dir.path).map_err(fs_error)?;
    if m.mode() & 0o777 != 0o700 {
        return Err("unsafe_organization");
    }
    Ok(())
}
fn private_file(file: &File) -> Result<()> {
    let m = file.metadata().map_err(fs_error)?;
    if !m.is_file()
        || m.nlink() != 1
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o777 != 0o600
    {
        return Err("unsafe_organization");
    }
    Ok(())
}
fn sidecars(dir: &Directory) -> Result<()> {
    for name in [
        "state.sqlite-journal",
        "state.sqlite-wal",
        "state.sqlite-shm",
        "bootstrap.lock",
    ] {
        match dir.open(name, false) {
            Ok(f) => private_file(&f)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(fs_error(e)),
        }
    }
    Ok(())
}
fn verify(files: &Files) -> Result<()> {
    private_dir(&files.root)?;
    private_dir(&files.directory)?;
    private_file(&files.file)?;
    if files.file.metadata().map_err(fs_error)?.len() > FILE_BYTES {
        return Err("organization_budget_exceeded");
    }
    if files
        .directory
        .entry_info(&files.name)
        .map_err(fs_error)?
        .identity
        != file_identity(&files.file)?
    {
        return Err("organization_unavailable");
    }
    sidecars(&files.directory)?;
    if files.name != "state.sqlite" {
        for suffix in ["-journal", "-wal", "-shm"] {
            match files
                .directory
                .open(&format!("{}{suffix}", files.name), false)
            {
                Ok(f) => private_file(&f)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(fs_error(e)),
            }
        }
    }
    Ok(())
}
fn file_identity(file: &File) -> Result<Identity> {
    let m = file.metadata().map_err(fs_error)?;
    Ok(Identity {
        device: m.dev(),
        inode: m.ino(),
    })
}
fn observation(files: &Files, store_id: String) -> Result<Seen> {
    Ok(Seen {
        root: files.root.identity().map_err(fs_error)?,
        directory: files.directory.identity().map_err(fs_error)?,
        file: file_identity(&files.file)?,
        store_id,
    })
}
fn check_seen(seen: &Option<Seen>, current: &Seen) -> Result<()> {
    if seen.as_ref().is_some_and(|old| old != current) {
        Err("organization_unavailable")
    } else {
        Ok(())
    }
}
fn connect(files: &Files, writable: bool) -> Result<Connection> {
    verify(files)?;
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let path = files
        .directory
        .path
        .canonicalize()
        .map_err(fs_error)?
        .join(&files.name);
    let db = Connection::open_with_flags(path, flags).map_err(|_| "organization_unavailable")?;
    db.busy_timeout(BUSY)
        .map_err(|_| "organization_unavailable")?;
    db.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, BYTES as i32)
        .map_err(|_| "organization_unavailable")?;
    db.execute_batch(
        "PRAGMA synchronous=FULL; PRAGMA trusted_schema=OFF; PRAGMA temp_store=MEMORY;",
    )
    .map_err(|_| "organization_unavailable")?;
    let journal: String = db
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .map_err(|_| "organization_unavailable")?;
    if journal != "delete" {
        return Err("organization_unavailable");
    }
    verify(files)?;
    Ok(db)
}
fn revision_number(revision: &str) -> Result<u64> {
    revision
        .rsplit_once(':')
        .and_then(|(_, r)| r.parse().ok())
        .ok_or("organization_unavailable")
}
fn validate_row(row: &Override) -> Result<()> {
    let text_ok = |s: &str| !s.is_empty() && s.len() <= FIELD && !s.contains('\0');
    if [&row.source_identity, &row.identity_digest]
        .iter()
        .any(|s| !text_ok(s))
    {
        return Err(
            if row.source_identity.len() > BYTES || row.identity_digest.len() > BYTES {
                "organization_budget_exceeded"
            } else {
                "organization_unavailable"
            },
        );
    }
    match row.disposition.as_str() {
        "project"
            if row.project_id.as_deref().is_some_and(text_ok)
                && row.project_path.as_deref().is_some_and(|s| {
                    let bytes = s.as_bytes();
                    text_ok(s)
                        && (s.starts_with('/')
                            || s.starts_with("\\\\")
                            || (bytes.first().is_some_and(u8::is_ascii_alphabetic)
                                && bytes.get(1) == Some(&b':')
                                && matches!(bytes.get(2), Some(b'/' | b'\\'))))
                }) =>
        {
            Ok(())
        }
        "unassigned" if row.project_id.is_none() && row.project_path.is_none() => Ok(()),
        _ => Err("organization_unavailable"),
    }
}
fn budget(rows: &BTreeMap<String, Override>) -> Result<()> {
    if rows.len() > ROWS
        || serde_json::to_vec(rows)
            .map_err(|_| "organization_unavailable")?
            .len()
            > BYTES
    {
        Err("organization_budget_exceeded")
    } else {
        Ok(())
    }
}
fn load(db: &Connection) -> Result<(Snapshot, String)> {
    let version: i64 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| "organization_unavailable")?;
    if version != 1 {
        return Err("organization_unavailable");
    }
    let tables: i64 = db.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('organization_meta','project_overrides') AND sql LIKE 'CREATE TABLE %'", [], |r| r.get(0)).map_err(|_| "organization_unavailable")?;
    let triggers: i64 = db.query_row("SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND tbl_name IN ('organization_meta','project_overrides')", [], |r| r.get(0)).map_err(|_| "organization_unavailable")?;
    if tables != 2 || triggers != 0 {
        return Err("organization_unavailable");
    }
    let meta_count: i64 = db
        .query_row("SELECT count(*) FROM organization_meta", [], |r| r.get(0))
        .map_err(|_| "organization_unavailable")?;
    if meta_count != 1 {
        return Err("organization_unavailable");
    }
    let mut meta = db
        .prepare("SELECT singleton,schema_version,store_id,revision FROM organization_meta WHERE length(CAST(store_id AS BLOB))<=4096 LIMIT 2")
        .map_err(|_| "organization_unavailable")?;
    let mut rows = meta.query([]).map_err(|_| "organization_unavailable")?;
    let row = rows
        .next()
        .map_err(|_| "organization_unavailable")?
        .ok_or("organization_unavailable")?;
    let singleton: i64 = row.get(0).map_err(|_| "organization_unavailable")?;
    let schema: i64 = row.get(1).map_err(|_| "organization_unavailable")?;
    let store_id: String = row.get(2).map_err(|_| "organization_unavailable")?;
    let revision: i64 = row.get(3).map_err(|_| "organization_unavailable")?;
    if singleton != 1
        || schema != 1
        || revision < 1
        || uuid::Uuid::parse_str(&store_id)
            .ok()
            .is_none_or(|id| id.is_nil())
        || rows
            .next()
            .map_err(|_| "organization_unavailable")?
            .is_some()
    {
        return Err("organization_unavailable");
    }
    let (count, bytes): (i64, i64) = db.query_row("SELECT count(*),coalesce(sum(length(CAST(entry_id AS BLOB))+length(CAST(source_identity AS BLOB))+length(CAST(identity_digest AS BLOB))+length(CAST(disposition AS BLOB))+coalesce(length(CAST(project_id AS BLOB)),0)+coalesce(length(CAST(project_path AS BLOB)),0)),0) FROM project_overrides", [], |r| Ok((r.get(0)?,r.get(1)?))).map_err(|_| "organization_unavailable")?;
    if count > ROWS as i64 || bytes > BYTES as i64 {
        return Err("organization_budget_exceeded");
    }
    let invalid: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM project_overrides WHERE length(CAST(entry_id AS BLOB))>4096 OR length(CAST(source_identity AS BLOB))>4096 OR length(CAST(identity_digest AS BLOB))>4096 OR length(CAST(disposition AS BLOB))>32 OR length(CAST(project_id AS BLOB))>4096 OR length(CAST(project_path AS BLOB))>4096)", [], |r| r.get(0)).map_err(|_| "organization_unavailable")?;
    if invalid {
        return Err("organization_unavailable");
    }
    let mut stmt = db.prepare("SELECT entry_id,source_identity,identity_digest,disposition,project_id,project_path FROM project_overrides ORDER BY entry_id LIMIT 1025").map_err(|_| "organization_unavailable")?;
    let mapped = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Override {
                    source_identity: r.get(1)?,
                    identity_digest: r.get(2)?,
                    disposition: r.get(3)?,
                    project_id: r.get(4)?,
                    project_path: r.get(5)?,
                },
            ))
        })
        .map_err(|_| "organization_unavailable")?;
    let mut overrides = BTreeMap::new();
    for row in mapped {
        let (entry, value) = row.map_err(|_| "organization_unavailable")?;
        if entry.is_empty() || entry.contains('\0') {
            return Err("organization_unavailable");
        }
        validate_row(&value)?;
        overrides.insert(entry, value);
    }
    budget(&overrides)?;
    if overrides.len() != count as usize {
        return Err("organization_unavailable");
    }
    Ok((
        Snapshot {
            revision: format!("{store_id}:{revision}"),
            overrides,
        },
        store_id,
    ))
}
struct Bootstrap(File);
impl Bootstrap {
    fn acquire(file: File) -> Result<Self> {
        let start = Instant::now();
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            let e = std::io::Error::last_os_error();
            if !matches!(e.raw_os_error(), Some(libc::EWOULDBLOCK | libc::EINTR))
                || start.elapsed() >= BUSY
            {
                return Err("organization_unavailable");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for Bootstrap {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(test)]
#[path = "organization_tests.rs"]
mod tests;
