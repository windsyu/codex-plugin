//! Disk reads and the disposable SQLite catalogue have their own bounded worker.
//! HTTP handlers only enqueue queries; they never traverse the native store.
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::oneshot;
use uuid::Uuid;

use super::fs::Directory;
use super::journal::{self, Meta};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryError {
    Busy,
    Unavailable,
    NotFound,
    InvalidCursor,
    StaleCursor,
    Deleted,
}
pub enum Query {
    List {
        cursor: Option<String>,
    },
    Run {
        epoch: Uuid,
    },
    Status {
        epoch: Uuid,
    },
    Earlier {
        epoch: Uuid,
        before: u64,
    },
    Details {
        epoch: Uuid,
        request: Uuid,
        cursor: Option<String>,
        before: Option<u64>,
    },
}
struct Job {
    query: Query,
    reply: oneshot::Sender<Result<Value, HistoryError>>,
}
#[derive(Clone)]
pub struct History {
    sender: Arc<mpsc::SyncSender<Job>>,
    pub current: Uuid,
    pub(crate) root: PathBuf,
    pub(crate) workspace: String,
}
impl History {
    pub fn start(root: PathBuf, workspace: String, current: Uuid) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(8);
        let worker_root = root.clone();
        let worker_workspace = workspace.clone();
        std::thread::Builder::new()
            .name("workbench-history".into())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    if job.reply.is_closed() {
                        continue;
                    }
                    let result = execute(&worker_root, &worker_workspace, current, job.query);
                    let _ = job.reply.send(result);
                }
            })?;
        Ok(Self {
            sender: Arc::new(sender),
            current,
            root,
            workspace,
        })
    }
    pub async fn query(&self, query: Query) -> Result<Value, HistoryError> {
        let (reply, response) = oneshot::channel();
        self.sender
            .try_send(Job { query, reply })
            .map_err(|_| HistoryError::Busy)?;
        tokio::time::timeout(Duration::from_secs(3), response)
            .await
            .map_err(|_| HistoryError::Busy)?
            .map_err(|_| HistoryError::Unavailable)?
    }
}
fn summary(meta: &Meta, active: bool) -> Value {
    json!({"runEpoch":meta.run_epoch,"projectName":meta.project_name,"startedAt":meta.started_at,
        "state":if active {"active"} else if meta.ended {"ended"} else {"unclean"},
        "savedThroughViewSeq":meta.saved_through_view_seq,"persistedThroughViewSeq":meta.persisted_through_view_seq,
        "gapCount":meta.gaps.len(),"historyCoverage":if meta.gaps.is_empty() {"complete_for_observed_scope"} else {"partial"}})
}
fn execute(
    root: &std::path::Path,
    workspace: &str,
    current: Uuid,
    query: Query,
) -> Result<Value, HistoryError> {
    let root = Directory::root(root).map_err(|_| HistoryError::Unavailable)?;
    let runs = root
        .dir("runs", true)
        .map_err(|_| HistoryError::Unavailable)?;
    if let Query::List { cursor } = query {
        return catalogue(&root, &runs, workspace, current, cursor.as_deref());
    }
    let epoch = match &query {
        Query::Run { epoch }
        | Query::Status { epoch }
        | Query::Earlier { epoch, .. }
        | Query::Details { epoch, .. } => *epoch,
        _ => unreachable!(),
    };
    let dir = runs.dir(&epoch.to_string(), false).map_err(|_| {
        if super::management::known_removed(&root, workspace, epoch) {
            HistoryError::Deleted
        } else {
            HistoryError::NotFound
        }
    })?;
    let meta = journal::read_meta(&dir, epoch).map_err(|_| HistoryError::Unavailable)?;
    if meta.workspace_id != workspace {
        return Err(HistoryError::NotFound);
    }
    let active = journal::active(&dir).map_err(|_| HistoryError::Unavailable)?;
    if matches!(query, Query::Status { .. }) {
        return Ok(json!({"currentRunEpoch":current,"run":summary(&meta,active)}));
    }
    let before = match &query {
        Query::Earlier { before, .. } => Some(*before),
        Query::Details { before, .. } => *before,
        _ => None,
    };
    if before.is_some_and(|v| v == 0 || v > meta.saved_through_view_seq) {
        return Err(HistoryError::InvalidCursor);
    }
    let restored = journal::recover(&dir, &meta, before).map_err(|_| HistoryError::Unavailable)?;
    if let Query::Details {
        request, cursor, ..
    } = query
    {
        return details(
            &meta,
            &restored.checkpoint,
            request,
            cursor.as_deref(),
            !restored.issues.is_empty(),
        );
    }
    let mut snapshot = restored.checkpoint.snapshot;
    let previous_before = restored.checkpoint.earlier_before;
    let partial = !meta.gaps.is_empty()
        || !restored.issues.is_empty()
        || !meta.ended
        || restored.checkpoint.details_partial;
    snapshot["recorder"] = json!(if partial { "degraded" } else { "saved" });
    snapshot["persistedThroughViewSeq"] = json!(
        meta.persisted_through_view_seq
            .min(restored.verified_prefix)
    );
    snapshot["historyCoverage"] = json!(if partial {
        "partial"
    } else {
        "complete_for_observed_scope"
    });
    snapshot["recorderStatus"] = json!({
        "runEpoch":epoch,"state":snapshot["recorder"],
        "persistedThroughViewSeq":snapshot["persistedThroughViewSeq"],
        "savedThroughViewSeq":snapshot["viewSeq"],"observedViewSeq":snapshot["viewSeq"],
        "historyCoverage":snapshot["historyCoverage"],"gapCount":meta.gaps.len(),
        "error":if partial {Some("historical_partial")} else {None}
    });
    Ok(
        json!({"run":summary(&meta,active),"snapshot":snapshot,"gaps":meta.gaps,"issues":restored.issues,"previousBefore":previous_before,"before":before,
        "uncertainTail":!active && !meta.ended,"detailsPartial":restored.checkpoint.details_partial,
        "source":"saved_workbench","currentRunEpoch":current}),
    )
}
fn catalogue(
    root: &Directory,
    runs: &Directory,
    workspace: &str,
    current: Uuid,
    cursor: Option<&str>,
) -> Result<Value, HistoryError> {
    let mut rows = Vec::new();
    let mut diagnostics = Vec::new();
    for (index, entry) in std::fs::read_dir(&runs.path)
        .map_err(|_| HistoryError::Unavailable)?
        .enumerate()
    {
        if index >= 10_000 {
            return Err(HistoryError::Unavailable);
        }
        let Ok(entry) = entry else {
            continue;
        };
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            continue;
        };
        let result = runs.dir(&id.to_string(), false).and_then(|dir| {
            journal::read_meta(&dir, id)
                .and_then(|meta| journal::active(&dir).map(|active| (meta, active)))
        });
        match result {
            Ok((meta, active)) if meta.workspace_id == workspace => {
                rows.push(summary(&meta, active))
            }
            Ok(_) => {}
            Err(_) => diagnostics.push(json!({"runEpoch":id,"code":"run_unavailable"})),
        }
    }
    rows.sort_by(|a, b| {
        b["startedAt"]
            .as_str()
            .cmp(&a["startedAt"].as_str())
            .then_with(|| b["runEpoch"].as_str().cmp(&a["runEpoch"].as_str()))
    });
    // The catalogue is a cache rebuilt from committed manifests every query.
    // Deleting/corrupting it never supplies fabricated history or loses source data.
    let generation = blake3::hash(
        rows.iter()
            .map(|r| r["runEpoch"].as_str().unwrap_or(""))
            .collect::<String>()
            .as_bytes(),
    )
    .to_hex()
    .to_string();
    let offset = if let Some(cursor) = cursor {
        let (version, offset) = cursor.split_once('.').ok_or(HistoryError::InvalidCursor)?;
        if version != generation {
            return Err(HistoryError::StaleCursor);
        }
        offset
            .parse::<usize>()
            .map_err(|_| HistoryError::InvalidCursor)?
    } else {
        0
    };
    if offset > rows.len() {
        return Err(HistoryError::InvalidCursor);
    }
    let end = (offset + 20).min(rows.len());
    let indexed = rebuild_index(root, workspace, &rows, offset);
    let mut page = indexed
        .as_ref()
        .unwrap_or(&rows[offset..end].to_vec())
        .clone();
    for row in &mut page {
        if let Some(id) = row["runEpoch"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            row["storage"] =
                serde_json::to_value(super::management::cached(root, runs, workspace, id))
                    .unwrap_or(Value::Null);
        }
    }
    Ok(
        json!({"currentRunEpoch":current,"runs":page,"nextCursor":(end < rows.len()).then(||format!("{generation}.{end}")),"diagnostics":diagnostics,"indexState":if indexed.is_ok() {"ready"} else {"unavailable"}}),
    )
}
fn rebuild_index(
    root: &Directory,
    workspace: &str,
    rows: &[Value],
    offset: usize,
) -> anyhow::Result<Vec<Value>> {
    // A fresh database avoids trusting SQL/schema in a corrupt index and uses a
    // private create_new file. A single atomic replace installs the rebuilt index.
    let temporary = format!("index-{}.sqlite", Uuid::new_v4());
    drop(root.open(&temporary, true)?);
    let result = (|| -> anyhow::Result<Vec<Value>> {
        let mut db = rusqlite::Connection::open(root.path.join(&temporary))?;
        db.execute_batch("PRAGMA journal_mode=MEMORY; PRAGMA synchronous=FULL; PRAGMA user_version=1; CREATE TABLE runs (workspace TEXT NOT NULL, epoch TEXT PRIMARY KEY, started TEXT NOT NULL, summary TEXT NOT NULL);")?;
        let tx = db.transaction()?;
        for row in rows {
            tx.execute(
                "INSERT INTO runs VALUES (?1,?2,?3,?4)",
                rusqlite::params![
                    workspace,
                    row["runEpoch"].as_str(),
                    row["startedAt"].as_str(),
                    row.to_string()
                ],
            )?;
        }
        tx.commit()?;
        let page = {
            let mut statement = db.prepare("SELECT summary FROM runs WHERE workspace=?1 ORDER BY started DESC, epoch DESC LIMIT 20 OFFSET ?2")?;
            let rows = statement.query_map(rusqlite::params![workspace, offset], |row| {
                row.get::<_, String>(0)
            })?;
            let mut page = Vec::new();
            for row in rows {
                page.push(serde_json::from_str(&row?)?);
            }
            page
        };
        db.close().map_err(|(_, e)| e)?;
        root.atomic(
            "index.sqlite",
            &root.read(&temporary, super::fs::FILE_LIMIT)?,
        )?;
        Ok(page)
    })();
    let _ = root.remove(&temporary);
    result
}
fn details(
    meta: &Meta,
    checkpoint: &super::replay::Checkpoint,
    request: Uuid,
    cursor: Option<&str>,
    damaged: bool,
) -> Result<Value, HistoryError> {
    let revision = checkpoint.record_seq;
    let prefix = format!("{}.{}.{revision}", meta.run_epoch, request);
    let offset = if let Some(cursor) = cursor {
        let (key, offset) = cursor.rsplit_once('.').ok_or(HistoryError::InvalidCursor)?;
        if key != prefix {
            return Err(HistoryError::StaleCursor);
        }
        offset
            .parse::<usize>()
            .map_err(|_| HistoryError::InvalidCursor)?
    } else {
        0
    };
    let documents: Vec<_> = checkpoint
        .documents
        .iter()
        .filter(|d| d.request_id == request)
        .collect();
    let known = checkpoint.snapshot["requests"]
        .as_array()
        .is_some_and(|a| a.iter().any(|r| r["requestId"] == request.to_string()));
    if !known && documents.is_empty() {
        return Err(HistoryError::NotFound);
    }
    let mut entries = Vec::new();
    let mut conflict = false;
    for (i, d) in documents.iter().enumerate() {
        conflict |= documents[..i].iter().any(|old| {
            old.document["source"] == d.document["source"] && old.document != d.document
        });
        if let Some(parts) = d.document["entries"].as_array() {
            for part in parts {
                let mut entry = part.clone();
                entry["source"] = d.document["source"].clone();
                entry["captureSeq"] = json!(d.capture_seq);
                entries.push(entry);
            }
        }
    }
    if offset > entries.len() {
        return Err(HistoryError::InvalidCursor);
    }
    let mut end = offset;
    let mut bytes = 1024;
    while end < entries.len() && end - offset < 16 {
        let cost = entries[end].to_string().len();
        if bytes + cost > 128 * 1024 {
            break;
        }
        bytes += cost;
        end += 1;
    }
    Ok(
        json!({"runEpoch":meta.run_epoch,"requestId":request,"revision":revision,
        "availability":if documents.is_empty(){"unavailable"}else{"captured"},
        "requestCaptured":documents.iter().any(|d|d.document["source"]["kind"]=="request"),
        "responseCaptured":documents.iter().any(|d|d.document["source"]["kind"]=="response"),
        "truncated":checkpoint.details_partial||documents.iter().any(|d|d.document["truncated"]==true),
        "omitted":documents.iter().any(|d|d.document["omitted"]==true),"conflict":conflict,
        "captureIssues":if damaged||!meta.gaps.is_empty(){vec!["history_gap"]}else{Vec::<&str>::new()},
        "totalEntries":entries.len(),"entries":&entries[offset..end],"nextCursor":(end<entries.len()).then(||format!("{prefix}.{end}"))}),
    )
}
