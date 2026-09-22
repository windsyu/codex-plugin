//! Read-only project queries, isolated from the PTY, proxy and recorder.
//! Paths and child processes are rooted in the launcher's pinned directory.
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::oneshot;

mod fs;
mod git;
mod process;
mod search;
#[cfg(test)]
mod tests;

const FILE_LIMIT: usize = 1024 * 1024;
const PAGE: usize = 200;
const DEADLINE: Duration = Duration::from_secs(5);
type Result<T> = std::result::Result<T, Fault>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault(pub &'static str);
impl From<std::io::Error> for Fault {
    fn from(e: std::io::Error) -> Self {
        Self(match e.kind() {
            std::io::ErrorKind::NotFound => "not_found",
            std::io::ErrorKind::PermissionDenied => "forbidden_path",
            _ => "workspace_io_error",
        })
    }
}

pub enum Query {
    Files {
        path: String,
        cursor: Option<String>,
    },
    File {
        path: String,
    },
    Search {
        text: String,
        regex: bool,
        case_sensitive: bool,
    },
    GitStatus,
    GitDiff {
        path: String,
        staged: bool,
    },
    GitLog {
        cursor: Option<String>,
    },
}
struct Work {
    query: Query,
    budget: Budget,
    reply: oneshot::Sender<Result<Value>>,
}
struct Budget {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}
impl Budget {
    fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(Fault("cancelled"))
        } else if Instant::now() >= self.deadline {
            Err(Fault("workspace_timeout"))
        } else {
            Ok(())
        }
    }
}
struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub struct Handle {
    pub root: String,
    tx: mpsc::SyncSender<Work>,
}
impl Handle {
    pub fn start(root: &Path) -> std::io::Result<Self> {
        let reader = Reader {
            root: fs::Root::open(root)?,
            programs: process::Programs::discover(),
        };
        Self::from_reader(root, reader)
    }
    fn from_reader(root: &Path, reader: Reader) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Work>(8);
        std::thread::Builder::new()
            .name("workbench-workspace".into())
            .spawn(move || {
                while let Ok(work) = rx.recv() {
                    if work.reply.is_closed() {
                        continue;
                    }
                    let result = work
                        .budget
                        .check()
                        .and_then(|()| reader.query(work.query, &work.budget));
                    let _ = work.reply.send(result);
                }
            })?;
        Ok(Self {
            root: root.to_string_lossy().into_owned(),
            tx,
        })
    }
    pub async fn query(&self, query: Query) -> Result<Value> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = Cancel(cancelled.clone());
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(Work {
                query,
                budget: Budget {
                    deadline: Instant::now() + DEADLINE,
                    cancelled,
                },
                reply,
            })
            .map_err(|_| Fault("workspace_busy"))?;
        tokio::time::timeout(DEADLINE + Duration::from_millis(100), rx)
            .await
            .map_err(|_| Fault("workspace_timeout"))?
            .map_err(|_| Fault("workspace_unavailable"))?
    }
}
struct Reader {
    root: fs::Root,
    programs: process::Programs,
}
impl Reader {
    fn query(&self, query: Query, budget: &Budget) -> Result<Value> {
        let started = Instant::now();
        let mut value = match query {
            Query::Files { path, cursor } => self.root.list(&path, cursor.as_deref(), budget),
            Query::File { path } => self.root.read(&path).map(|text| json!({"path":path,"text":text,"revision":blake3::hash(text.as_bytes()).to_hex().as_str(),"truncated":false})),
            Query::Search { text, regex, case_sensitive } => self.search(&text, regex, case_sensitive, budget),
            Query::GitStatus => self.git_status(budget),
            Query::GitDiff { path, staged } => self.git_diff(&path, staged, budget),
            Query::GitLog { cursor } => self.git_log(cursor.as_deref(), budget),
        }?;
        if budget.cancelled.load(Ordering::Relaxed) {
            return Err(Fault("cancelled"));
        }
        value["readAt"] = json!(chrono::Utc::now().to_rfc3339());
        value["elapsedMs"] = json!(started.elapsed().as_millis() as u64);
        Ok(value)
    }
}
