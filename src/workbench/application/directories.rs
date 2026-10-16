//! Owner-only, bounded directory metadata browsing; never reads file contents.
use super::launching::DirectoryGuard;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use uuid::Uuid;

const PAGE_SIZE: usize = 100;
const MAX_SCANNED: usize = 4096;
const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_SNAPSHOTS: usize = 8;
const SNAPSHOT_TTL: Duration = Duration::from_secs(60);
const SCAN_TIME: Duration = Duration::from_millis(750);
const REQUEST_TIME: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BrowseRequest {
    pub instance_id: Uuid,
    pub path: Option<String>,
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default)]
    pub filter: String,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectoryEntry {
    pub name: String,
    pub path: String,
    pub is_symlink: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectoryRoot {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowseResponse {
    pub instance_id: Uuid,
    pub current_path: String,
    pub parent_path: Option<String>,
    pub home_path: String,
    pub roots: Vec<DirectoryRoot>,
    pub entries: Vec<DirectoryEntry>,
    pub can_select: bool,
    pub selection_error: Option<&'static str>,
    pub next_cursor: Option<String>,
    pub truncated: bool,
}

#[derive(Clone)]
pub(crate) struct DirectoryBrowser {
    home: PathBuf,
    // try_acquire prevents a request queue. The owned permit stays inside the
    // blocking task even after its HTTP caller times out or is revoked.
    scans: Arc<Semaphore>,
    snapshots: Arc<Mutex<HashMap<Uuid, Arc<Snapshot>>>>,
}

struct Snapshot {
    instance_id: Uuid,
    path: String,
    filter: String,
    show_hidden: bool,
    stamp: Stamp,
    created: Instant,
    entries: Vec<DirectoryEntry>,
    truncated: bool,
    can_select: bool,
    selection_error: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Stamp {
    fn of(metadata: &std::fs::Metadata) -> Result<Self, &'static str> {
        if !metadata.is_dir() {
            return Err("directory_unavailable");
        }
        Ok(Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

impl DirectoryBrowser {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            scans: Arc::new(Semaphore::new(2)),
            snapshots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn list(
        &self,
        request: BrowseRequest,
        instance_id: Uuid,
    ) -> Result<BrowseResponse, &'static str> {
        if instance_id.is_nil() || request.instance_id != instance_id {
            return Err("stale_instance");
        }
        if request.filter.len() > 256 || request.filter.chars().any(char::is_control) {
            return Err("invalid_directory_filter");
        }
        if let Some(path) = &request.path {
            validate_path(path)?;
        }
        if let Some(cursor) = &request.cursor {
            parse_cursor(cursor)?;
        }
        let browser = self.clone();
        self.blocking(move || browser.list_blocking(request, instance_id))
            .await
    }

    async fn blocking(
        &self,
        scan: impl FnOnce() -> Result<BrowseResponse, &'static str> + Send + 'static,
    ) -> Result<BrowseResponse, &'static str> {
        let permit = self
            .scans
            .clone()
            .try_acquire_owned()
            .map_err(|_| "directory_busy")?;
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            scan()
        });
        tokio::time::timeout(REQUEST_TIME, task)
            .await
            .map_err(|_| "directory_busy")?
            .map_err(|_| "directory_unavailable")?
    }

    fn list_blocking(
        &self,
        request: BrowseRequest,
        instance_id: Uuid,
    ) -> Result<BrowseResponse, &'static str> {
        let home = self.home.canonicalize().map_err(io_error)?;
        let home_path = path_text(&home)?;
        let requested = match request.path.as_deref() {
            None | Some("~") => home.clone(),
            Some(value) if value.starts_with("~/") => home.join(&value[2..]),
            Some(value) => PathBuf::from(value),
        };
        if !requested.is_absolute() {
            return Err("absolute_path_required");
        }
        let path = requested.canonicalize().map_err(io_error)?;
        let path_string = path_text(&path)?;
        // Holding a descriptor lets us detect replacement during enumeration.
        let file = open_directory(&path)?;
        let stamp = Stamp::of(&file.metadata().map_err(io_error)?)?;
        let (id, offset, snapshot) = if let Some(cursor) = &request.cursor {
            let (id, offset) = parse_cursor(cursor)?;
            let snapshot = {
                let mut snapshots = self.snapshots.lock().unwrap();
                snapshots.retain(|_, entry| entry.created.elapsed() < SNAPSHOT_TTL);
                snapshots
                    .get(&id)
                    .cloned()
                    .ok_or("directory_cursor_expired")?
            };
            if snapshot.instance_id != instance_id
                || snapshot.path != path_string
                || snapshot.filter != request.filter
                || snapshot.show_hidden != request.show_hidden
                || offset == 0
                || offset >= snapshot.entries.len()
            {
                return Err("invalid_directory_cursor");
            }
            if snapshot.stamp != stamp {
                return Err("directory_changed");
            }
            (id, offset, snapshot)
        } else {
            let (entries, truncated) = scan(&path, &request)?;
            verify_directory(&path, &file, stamp)?;
            let selection = DirectoryGuard::open(&path).and_then(|guard| guard.verify());
            let snapshot = Arc::new(Snapshot {
                instance_id,
                path: path_string,
                filter: request.filter,
                show_hidden: request.show_hidden,
                stamp,
                created: Instant::now(),
                entries,
                truncated,
                can_select: selection.is_ok(),
                selection_error: selection.err(),
            });
            let id = Uuid::new_v4();
            let mut snapshots = self.snapshots.lock().unwrap();
            snapshots.retain(|_, entry| entry.created.elapsed() < SNAPSHOT_TTL);
            if snapshots.len() >= MAX_SNAPSHOTS
                && let Some(oldest) = snapshots
                    .iter()
                    .min_by_key(|(_, entry)| entry.created)
                    .map(|(id, _)| *id)
            {
                snapshots.remove(&oldest);
            }
            snapshots.insert(id, snapshot.clone());
            (id, 0, snapshot)
        };
        verify_directory(&path, &file, stamp)?;
        let mut roots = vec![
            DirectoryRoot {
                name: "~".into(),
                path: home_path.clone(),
            },
            DirectoryRoot {
                name: "/".into(),
                path: "/".into(),
            },
        ];
        if Path::new("/Volumes").is_dir() {
            roots.push(DirectoryRoot {
                name: "/Volumes".into(),
                path: "/Volumes".into(),
            });
        }
        let mut response = BrowseResponse {
            instance_id,
            current_path: snapshot.path.clone(),
            parent_path: path.parent().map(path_text).transpose()?,
            home_path,
            roots,
            entries: vec![],
            can_select: snapshot.can_select,
            selection_error: snapshot.selection_error,
            // Reserve space for the longest possible cursor before paging.
            next_cursor: Some(format!("{id}:{MAX_SCANNED}")),
            truncated: snapshot.truncated,
        };
        let mut bytes = serde_json::to_vec(&response)
            .map_err(|_| "directory_unavailable")?
            .len();
        for entry in snapshot.entries.iter().skip(offset).take(PAGE_SIZE) {
            let size = serde_json::to_vec(entry)
                .map_err(|_| "directory_unavailable")?
                .len()
                + 1;
            if bytes + size > MAX_RESPONSE_BYTES {
                break;
            }
            bytes += size;
            response.entries.push(entry.clone());
        }
        let next = offset + response.entries.len();
        response.next_cursor = (next < snapshot.entries.len()).then(|| format!("{id}:{next}"));
        Ok(response)
    }
}

fn validate_path(value: &str) -> Result<(), &'static str> {
    if value.is_empty()
        || value.len() > 4096
        || value.chars().any(char::is_control)
        || Path::new(value).ancestors().count() > 64
    {
        Err("invalid_directory_path")
    } else {
        Ok(())
    }
}

fn path_text(path: &Path) -> Result<String, &'static str> {
    let value = path.to_str().ok_or("invalid_directory_path")?;
    validate_path(value)?;
    Ok(value.to_owned())
}

fn parse_cursor(value: &str) -> Result<(Uuid, usize), &'static str> {
    if value.len() > 48 {
        return Err("invalid_directory_cursor");
    }
    let (id, offset) = value.split_once(':').ok_or("invalid_directory_cursor")?;
    let parsed_id = Uuid::parse_str(id).map_err(|_| "invalid_directory_cursor")?;
    let parsed_offset = offset
        .parse::<usize>()
        .map_err(|_| "invalid_directory_cursor")?;
    if parsed_id.is_nil()
        || parsed_id.to_string() != id
        || parsed_offset.to_string() != offset
        || parsed_offset == 0
        || parsed_offset >= MAX_SCANNED
    {
        return Err("invalid_directory_cursor");
    }
    Ok((parsed_id, parsed_offset))
}

fn io_error(error: io::Error) -> &'static str {
    if error.kind() == io::ErrorKind::PermissionDenied {
        "directory_forbidden"
    } else {
        "directory_unavailable"
    }
}

fn open_directory(path: &Path) -> Result<File, &'static str> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(io_error)
}

fn verify_directory(path: &Path, file: &File, stamp: Stamp) -> Result<(), &'static str> {
    let current = std::fs::symlink_metadata(path).map_err(|_| "directory_changed")?;
    let pinned = file.metadata().map_err(|_| "directory_changed")?;
    if Stamp::of(&current).map_err(|_| "directory_changed")? != stamp
        || Stamp::of(&pinned).map_err(|_| "directory_changed")? != stamp
    {
        Err("directory_changed")
    } else {
        Ok(())
    }
}

fn entry_name(value: std::ffi::OsString) -> Option<String> {
    value
        .into_string()
        .ok()
        .filter(|name| !name.chars().any(char::is_control))
}

fn scan(path: &Path, request: &BrowseRequest) -> Result<(Vec<DirectoryEntry>, bool), &'static str> {
    let started = Instant::now();
    let filter = request.filter.to_lowercase();
    let mut entries = vec![];
    let mut bytes = 0;
    let mut truncated = false;
    for (index, result) in std::fs::read_dir(path).map_err(io_error)?.enumerate() {
        if index >= MAX_SCANNED || started.elapsed() >= SCAN_TIME {
            truncated = true;
            break;
        }
        let Ok(entry) = result else {
            truncated = true;
            continue;
        };
        let Some(name) = entry_name(entry.file_name()) else {
            truncated = true;
            continue;
        };
        if (!request.show_hidden && name.starts_with('.'))
            || (!filter.is_empty() && !name.to_lowercase().contains(&filter))
        {
            continue;
        }
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(_) => {
                truncated = true;
                continue;
            }
        };
        if !(kind.is_dir()
            || kind.is_symlink() && entry.path().metadata().is_ok_and(|meta| meta.is_dir()))
        {
            continue;
        }
        let Ok(path) = path_text(&entry.path()) else {
            truncated = true;
            continue;
        };
        bytes += name.len() + path.len() + 64;
        if bytes > MAX_SNAPSHOT_BYTES {
            truncated = true;
            break;
        }
        entries.push(DirectoryEntry {
            name,
            path,
            is_symlink: kind.is_symlink(),
        });
    }
    entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Ok((entries, truncated))
}

#[cfg(test)]
#[path = "directories_tests.rs"]
mod tests;
