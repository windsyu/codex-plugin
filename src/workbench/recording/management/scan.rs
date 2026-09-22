pub(super) use super::super::lifecycle::Lifecycle;
use super::super::{
    fs::{Directory, Entries, Identity},
    journal,
};
use super::model::*;
use chrono::{DateTime, Utc};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub(super) fn lock(dir: &Directory, name: &str, create: bool) -> io::Result<File> {
    let f = if create {
        match dir.open(name, true) {
            Ok(f) => {
                dir.sync()?;
                f
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => dir.open(name, false)?,
            Err(e) => return Err(e),
        }
    } else {
        dir.open(name, false)?
    };
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(f)
}
pub(super) fn file_identity(file: &File) -> io::Result<Identity> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata()?;
    Ok(Identity {
        device: m.dev(),
        inode: m.ino(),
    })
}

pub(super) fn ended_at(
    dir: &Directory,
    meta: &journal::Meta,
    digest: &str,
    now: DateTime<Utc>,
) -> Result<String, &'static str> {
    if !meta.ended {
        return Err("unclean_run");
    }
    let bytes = dir.read("lifecycle.json", 4096).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            "ended_at_unknown"
        } else {
            "lifecycle_invalid"
        }
    })?;
    let proof: Lifecycle = serde_json::from_slice(&bytes).map_err(|_| "lifecycle_invalid")?;
    if proof.schema_version != 1
        || proof.run_epoch != meta.run_epoch
        || proof.workspace_id != meta.workspace_id
        || proof.final_meta_digest != digest
        || proof.saved_record_seq != meta.saved_record_seq
    {
        return Err("lifecycle_invalid");
    }
    let end = DateTime::parse_from_rfc3339(&proof.ended_at).map_err(|_| "lifecycle_invalid")?;
    let start = DateTime::parse_from_rfc3339(&meta.started_at).map_err(|_| "clock_invalid")?;
    if end < start || end > now {
        return Err("clock_invalid");
    }
    Ok(proof.ended_at)
}
pub(super) fn expired(row: &UsageRow, days: u32, now: DateTime<Utc>) -> bool {
    row.manual_eligible
        && row.state == "ended"
        && row.retention_reason.is_none()
        && row
            .ended_at
            .as_ref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .is_some_and(|t| now.signed_duration_since(t).num_seconds() >= i64::from(days) * 86400)
}
pub(super) fn allowed_file(name: &str, blobs: bool) -> bool {
    if let Some(id) = name.strip_prefix('.').and_then(|n| n.strip_suffix(".tmp")) {
        return Uuid::parse_str(id).is_ok();
    }
    if blobs {
        return name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit());
    }
    matches!(
        name,
        "lock" | "meta.json" | "snapshot.json" | "lifecycle.json"
    ) || name
        .strip_prefix("observations.")
        .and_then(|n| n.strip_suffix(".jsonl"))
        .is_some_and(|n| Uuid::parse_str(n).is_ok())
}

// A run has only one known child directory (blobs). Unknown trees are never
// followed. Each tick performs a bounded number of descriptor-relative stats.
pub(super) struct Measure {
    dir: Directory,
    entries: Entries,
    blobs: Option<(Directory, Entries)>,
    candidate: Candidate,
    held_lock: Option<File>,
    complete: bool,
    bytes: u64,
    contents: [u8; 32],
    file_count: u64,
}
impl Measure {
    pub fn begin(
        runs: &Directory,
        epoch: Uuid,
        workspace: &str,
        current: Uuid,
    ) -> io::Result<Option<Self>> {
        let dir = runs.dir(&epoch.to_string(), false)?;
        let meta = journal::read_meta(&dir, epoch)?;
        if meta.workspace_id != workspace {
            return Ok(None);
        }
        let identity = dir.identity()?;
        if identity.device != runs.identity()?.device {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let lock_file = dir.open("lock", false)?;
        let lock_identity = file_identity(&lock_file)?;
        let active =
            unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0;
        if active && io::Error::last_os_error().kind() != io::ErrorKind::WouldBlock {
            return Err(io::Error::last_os_error());
        }
        // Re-read after taking the lock; a writer may just have committed.
        let bytes = dir.read("meta.json", 4 * 1024 * 1024)?;
        let meta = journal::read_meta(&dir, epoch)?;
        if meta.workspace_id != workspace {
            return Ok(None);
        }
        let digest = blake3::hash(&bytes).to_hex().to_string();
        let end = ended_at(&dir, &meta, &digest, Utc::now());
        let reason = if epoch == current {
            Some("current_run")
        } else if active {
            Some("active_run")
        } else {
            None
        };
        let row = UsageRow {
            run_epoch: epoch,
            started_at: DateTime::parse_from_rfc3339(&meta.started_at)
                .map(|v| v.to_rfc3339())
                .unwrap_or_default(),
            ended_at: end.as_ref().ok().cloned(),
            state: if active || epoch == current {
                "active"
            } else if meta.ended {
                "ended"
            } else {
                "unclean"
            }
            .into(),
            size: Size::new(0, false),
            manual_eligible: reason.is_none(),
            reason: reason.map(str::to_owned),
            retention_reason: end.err().map(str::to_owned),
        };
        Ok(Some(Self {
            entries: dir.entries()?,
            dir,
            blobs: None,
            candidate: Candidate {
                row,
                identity,
                lock_identity,
                meta_digest: digest,
                contents_digest: String::new(),
                file_count: 0,
            },
            held_lock: (!active).then_some(lock_file),
            complete: true,
            bytes: 0,
            contents: [0; 32],
            file_count: 0,
        }))
    }
    pub fn step(&mut self, budget: usize) -> io::Result<bool> {
        let deadline = Instant::now() + Duration::from_millis(20);
        for _ in 0..budget {
            if Instant::now() >= deadline {
                return Ok(false);
            }
            let (dir, entry, in_blobs) = if let Some((dir, entries)) = &mut self.blobs {
                (&*dir, entries.next(), true)
            } else {
                (&self.dir, self.entries.next(), false)
            };
            let Some(entry) = entry else {
                if in_blobs {
                    self.blobs = None;
                    continue;
                }
                self.candidate.row.size = Size::new(self.bytes, self.complete);
                self.candidate.contents_digest =
                    self.contents.iter().map(|b| format!("{b:02x}")).collect();
                self.candidate.file_count = self.file_count;
                if !self.complete {
                    self.candidate.row.manual_eligible = false;
                    self.candidate.row.reason = Some("unsafe_or_unreadable_entry".into());
                }
                self.dir.verify_location()?;
                if let Some(lock) = &self.held_lock
                    && (file_identity(lock)? != self.dir.entry_info("lock")?.identity
                        || blake3::hash(&self.dir.read("meta.json", 4 * 1024 * 1024)?)
                            .to_hex()
                            .as_str()
                            != self.candidate.meta_digest)
                {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                return Ok(true);
            };
            let Ok(name) = entry else {
                self.complete = false;
                continue;
            };
            let Ok(info) = dir.entry_info(&name) else {
                self.complete = false;
                continue;
            };
            if info.identity.device != self.candidate.identity.device {
                self.complete = false;
                continue;
            }
            let path = if in_blobs {
                format!("blobs/{name}")
            } else {
                name.clone()
            };
            // Order-independent inventory digest; names, inode, size and mtime
            // must still match when a frozen preview is executed.
            let signature = serde_json::to_vec(&(
                path,
                info.identity,
                info.directory,
                if info.directory { 0 } else { info.bytes },
                if info.directory {
                    (0, 0)
                } else {
                    info.modified
                },
            ))?;
            for (a, b) in self
                .contents
                .iter_mut()
                .zip(blake3::hash(&signature).as_bytes())
            {
                *a ^= *b;
            }
            self.file_count += 1;
            if info.directory {
                if !in_blobs && name == "blobs" {
                    match dir
                        .dir(&name, false)
                        .and_then(|d| d.entries().map(|e| (d, e)))
                    {
                        Ok(value) => self.blobs = Some(value),
                        Err(_) => self.complete = false,
                    }
                } else {
                    self.complete = false;
                }
            } else {
                self.bytes = self
                    .bytes
                    .checked_add(info.bytes)
                    .ok_or(io::ErrorKind::InvalidData)?;
                if !allowed_file(&name, in_blobs) {
                    self.complete = false;
                }
            }
        }
        Ok(false)
    }
    pub fn finish(self) -> Candidate {
        self.candidate
    }
    pub fn finish_locked(self) -> (Candidate, Directory, Option<File>) {
        (self.candidate, self.dir, self.held_lock)
    }
}

pub(super) struct Scan {
    pub root: Directory,
    pub runs: Directory,
    pub cache: Directory,
    entries: Option<Entries>,
    measure: Option<Measure>,
    retry_measure: Option<Uuid>,
    file: File,
    end: u64,
    extras: Option<super::extras::Extras>,
    pub(super) scan_lock: Option<File>,
    pub info: Usage,
}
impl Scan {
    pub fn begin(root: &std::path::Path) -> io::Result<Self> {
        let root = Directory::root(root)?;
        let runs = root.dir("runs", false)?;
        let cache = root.dir("usage-v1", true)?;
        let lock = lock(&cache, "lock", true)?;
        let id = Uuid::new_v4();
        let temporary = format!(".{id}.tmp");
        let file = cache.open(&temporary, true)?;
        cache.remove(&temporary)?; // ephemeral pages; no cache file is authoritative
        let entries = runs.entries()?;
        let pending = match root.dir("cleanup", false) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Size::new(0, true),
            _ => Size::new(0, false),
        };
        let shared = root
            .entry_info("index.sqlite")
            .ok()
            .filter(|i| !i.directory)
            .map_or(0, |i| i.bytes);
        Ok(Self {
            root,
            runs,
            cache,
            entries: Some(entries),
            measure: None,
            retry_measure: None,
            file,
            end: 0,
            extras: None,
            scan_lock: Some(lock),
            info: Usage {
                scan_id: id,
                state: "scanning".into(),
                measured_at: Utc::now().to_rfc3339(),
                run_count: 0,
                history_bytes: 0,
                active_bytes: 0,
                unknown_runs: 0,
                unverified_entries: 0,
                examined_entries: 0,
                runs: vec![],
                next_cursor: None,
                pending_cleanup: pending,
                shared_management: Size::new(shared, false),
            },
        })
    }
    pub fn step(&mut self, workspace: &str, current: Uuid) -> io::Result<bool> {
        self.root.verify_location()?;
        self.runs.verify_location()?;
        if let Some(id) = self.retry_measure.take() {
            self.measure = Measure::begin(&self.runs, id, workspace, current)?;
        }
        if let Some(measure) = &mut self.measure {
            match measure.step(128) {
                Ok(false) => return Ok(false),
                Ok(true) => {
                    let candidate = self.measure.take().unwrap().finish();
                    let row = &candidate.row;
                    let mut bytes = serde_json::to_vec(row)?;
                    bytes.push(b'\n');
                    self.file.seek(SeekFrom::Start(self.end))?;
                    self.file.write_all(&bytes)?;
                    self.end += bytes.len() as u64;
                    self.info.run_count += 1;
                    if row.state == "active" {
                        self.info.active_bytes =
                            self.info.active_bytes.saturating_add(row.size.known_bytes);
                    } else {
                        self.info.history_bytes =
                            self.info.history_bytes.saturating_add(row.size.known_bytes);
                    }
                    if row.size.bytes.is_none() {
                        self.info.unknown_runs += 1;
                    }
                    let id = candidate.row.run_epoch;
                    let encoded = serde_json::to_vec(&CachedRow {
                        workspace_id: workspace.into(),
                        candidate,
                    })?;
                    let _ = self.cache.atomic(&format!("{id}.json"), &encoded);
                }
                Err(_) => {
                    self.measure = None;
                    self.info.unverified_entries += 1;
                }
            }
        }
        if self.entries.is_none() {
            if self.extras.is_none() {
                self.extras = Some(super::extras::Extras::new(&self.root)?);
            }
            let extras = self.extras.as_mut().unwrap();
            let complete = extras.step(&self.root, workspace)?;
            let (shared, pending) = extras.sizes();
            self.info.shared_management = shared;
            self.info.pending_cleanup = pending;
            if complete {
                self.info.state = if self.info.unverified_entries + self.info.unknown_runs == 0 {
                    "complete"
                } else {
                    "partial"
                }
                .into();
                self.info.measured_at = Utc::now().to_rfc3339();
                self.scan_lock = None;
            }
            return Ok(complete);
        }
        let entries = self.entries.as_mut().unwrap();
        // Continue the iterator across ticks instead of truncating at 10,000.
        let deadline = Instant::now() + Duration::from_millis(20);
        for _ in 0..128 {
            if Instant::now() >= deadline {
                return Ok(false);
            }
            let Some(entry) = entries.next() else {
                self.entries = None;
                return Ok(false);
            };
            self.info.examined_entries += 1;
            let Some(id) = entry
                .ok()
                .and_then(|s| Uuid::parse_str(&s).ok().filter(|id| id.to_string() == s))
            else {
                self.info.unverified_entries += 1;
                continue;
            };
            match Measure::begin(&self.runs, id, workspace, current) {
                Ok(Some(m)) => {
                    self.measure = Some(m);
                    return Ok(false);
                }
                Ok(None) => {}
                Err(_) => self.info.unverified_entries += 1,
            }
        }
        Ok(false)
    }
    pub fn pause(&mut self) {
        if let Some(m) = self.measure.take() {
            self.retry_measure = Some(m.candidate.row.run_epoch);
        }
    }
    pub fn page(&mut self, cursor: Option<&str>) -> Result<Usage, Error> {
        let offset = if let Some(cursor) = cursor {
            let (id, offset) = cursor.split_once('.').ok_or(Error::Invalid)?;
            if id != self.info.scan_id.to_string() {
                return Err(Error::Stale);
            }
            offset.parse::<u64>().map_err(|_| Error::Invalid)?
        } else {
            0
        };
        if offset > self.end {
            return Err(Error::Invalid);
        }
        // Reject a forged middle-of-line cursor rather than parsing fragments.
        if offset > 0 {
            use std::io::Read;
            self.file
                .seek(SeekFrom::Start(offset - 1))
                .map_err(|_| Error::Unavailable)?;
            let mut c = [0];
            self.file
                .read_exact(&mut c)
                .map_err(|_| Error::Unavailable)?;
            if c[0] != b'\n' {
                return Err(Error::Invalid);
            }
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|_| Error::Unavailable)?;
        let mut reader = BufReader::new(&self.file);
        let mut rows = Vec::new();
        let mut position = offset;
        for _ in 0..20 {
            if position >= self.end {
                break;
            }
            let mut line = String::new();
            let len = reader
                .read_line(&mut line)
                .map_err(|_| Error::Unavailable)?;
            if len == 0 || len > 8192 {
                return Err(Error::Unavailable);
            }
            rows.push(serde_json::from_str(&line).map_err(|_| Error::Unavailable)?);
            position += len as u64;
        }
        let mut info = self.info.clone();
        info.runs = rows;
        info.next_cursor = (position < self.end).then(|| format!("{}.{}", info.scan_id, position));
        Ok(info)
    }
}
pub(crate) fn cached(
    root: &Directory,
    runs: &Directory,
    workspace: &str,
    epoch: Uuid,
) -> Option<UsageRow> {
    let bytes = root
        .dir("usage-v1", false)
        .ok()?
        .read(&format!("{epoch}.json"), 8192)
        .ok()?;
    let row: CachedRow = serde_json::from_slice(&bytes).ok()?;
    if row.workspace_id != workspace
        || row.candidate.row.run_epoch != epoch
        || runs.dir(&epoch.to_string(), false).ok()?.identity().ok()? != row.candidate.identity
    {
        return None;
    }
    Some(row.candidate.row)
}
