//! Narrow read-only adapter; never invokes history index/rebuild or management.
use super::{fs::Directory, journal};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io, path::Path};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Project {
    pub format_version: u32,
    pub workspace_id: String,
    pub cwd: String,
    pub native_home: String,
}
type ReadResult = (Value, Vec<Value>, Option<Project>, Vec<String>, String);
pub(crate) fn read(root: &Path, epoch: uuid::Uuid) -> io::Result<ReadResult> {
    let directory = Directory::read_only(root)?;
    let runs = directory.dir("runs", false)?;
    let run = runs.dir(&epoch.to_string(), false)?;
    let meta = journal::read_meta(&run, epoch)?;
    let source_revision = materials_revision(&run, &meta)?;
    let project = run
        .read("project.json", 16 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice::<Project>(&b).ok())
        .filter(|p| {
            p.format_version == 1
                && p.workspace_id == meta.workspace_id
                && Path::new(&p.cwd).is_absolute()
                && blake3::hash(p.cwd.as_bytes()).to_hex().as_str() == meta.workspace_id
        });
    let mut issues = vec![];
    let mut records = meta
        .gaps
        .iter()
        .take(32)
        .map(|gap| json!({"kind":"history_gap","raw":gap}))
        .collect::<Vec<_>>();
    if project.is_none() {
        issues.push("project_path_not_recorded".into());
    }
    if !meta.gaps.is_empty() {
        issues.push("recording_gaps".into());
    }
    // Bound both I/O and allocations before using the verified journal replay.
    let blobs = run.dir("blobs", false)?;
    let mut total = 0u64;
    for item in run.entries()? {
        let item = item?;
        if item.starts_with("observations.") {
            total = total.saturating_add(run.entry_info(&item)?.bytes);
        }
    }
    for item in blobs.entries()? {
        total = total.saturating_add(blobs.entry_info(&item?)?.bytes);
        if total > 16 * 1024 * 1024 {
            break;
        }
    }
    if total > 16 * 1024 * 1024 {
        issues.push("run_read_budget".into());
    } else {
        let recovered = journal::recover(&run, &meta, None)?;
        if !recovered.issues.is_empty() {
            issues.push("journal_incomplete".into());
            records.extend(
                recovered
                    .issues
                    .iter()
                    .take(32)
                    .map(|issue| json!({"kind":"history_gap","raw":issue})),
            );
        }
        if recovered.checkpoint.earlier_before.is_some() {
            issues.push("earlier_window_not_cached".into());
        }
        records.push(json!({"kind":"workbench_snapshot","raw":recovered.checkpoint.snapshot}));
        for document in recovered.checkpoint.documents {
            records.push(json!({"kind":"call_detail","raw":document}));
        }
    }
    run.verify_location()?;
    if serde_json::to_vec(&meta).ok() != serde_json::to_vec(&journal::read_meta(&run, epoch)?).ok()
    {
        return Err(io::ErrorKind::Interrupted.into());
    }
    if materials_revision(&run, &meta)? != source_revision {
        return Err(io::ErrorKind::Interrupted.into());
    }
    Ok((json!(meta), records, project, issues, source_revision))
}

/// Read a selected saved window without enumerating/rebuilding the history
/// index. Independent segment checkpoints avoid replaying the whole run.
type WindowResult = (Vec<Value>, Option<u64>, Vec<String>);
pub(crate) fn window(
    root: &Path,
    epoch: uuid::Uuid,
    revision: &str,
    before: Option<u64>,
) -> Result<WindowResult, &'static str> {
    let directory = Directory::read_only(root).map_err(|_| "source_unavailable")?;
    let run = directory
        .dir("runs", false)
        .and_then(|d| d.dir(&epoch.to_string(), false))
        .map_err(|_| "source_unavailable")?;
    let meta = journal::read_meta(&run, epoch).map_err(|_| "source_unavailable")?;
    let signature = materials_revision(&run, &meta).map_err(|_| "source_revision_changed")?;
    if signature != revision {
        return Err("source_revision_changed");
    }
    let index = before
        .and_then(|limit| meta.segments.iter().position(|s| s.view_seq >= limit))
        .unwrap_or(meta.segments.len().saturating_sub(1));
    let mut selected = meta.clone();
    selected.segments = meta.segments.get(index).cloned().into_iter().collect();
    let recovered = journal::recover_bounded(
        &run,
        &selected,
        before,
        Some(std::time::Instant::now() + std::time::Duration::from_secs(1)),
    )
    .map_err(|e| {
        if e.kind() == io::ErrorKind::TimedOut {
            "source_read_budget"
        } else {
            "source_read_failed"
        }
    })?;
    let mut reasons = vec![];
    if !recovered.issues.is_empty() {
        reasons.push("journal_incomplete".into());
    }
    if recovered.checkpoint.details_partial {
        reasons.push("saved_details_partial".into());
    }
    let earlier = recovered
        .checkpoint
        .earlier_before
        .filter(|n| before.is_none_or(|b| *n < b));
    let mut records = meta
        .gaps
        .iter()
        .take(32)
        .map(|g| json!({"kind":"history_gap","raw":g}))
        .collect::<Vec<_>>();
    for issue in recovered.issues {
        records.push(json!({"kind":"history_gap","raw":issue}));
    }
    let snapshot = recovered.checkpoint.snapshot;
    let mut header = snapshot.clone();
    for field in [
        "items",
        "requests",
        "responses",
        "diagnostics",
        "toolContexts",
        "nativeCommands",
        "nativeFileChanges",
    ] {
        header.as_object_mut().unwrap().remove(field);
    }
    records.push(json!({"kind":"workbench_snapshot","raw":header}));
    for field in [
        "items",
        "requests",
        "responses",
        "diagnostics",
        "toolContexts",
        "nativeCommands",
        "nativeFileChanges",
    ] {
        if let Some(items) = snapshot[field].as_array() {
            records.extend(items.iter().map(|v| json!({"kind":field,"raw":v})));
        }
    }
    for document in recovered.checkpoint.documents {
        records.push(json!({"kind":"call_detail","raw":document}));
    }
    run.verify_location()
        .map_err(|_| "source_revision_changed")?;
    if serde_json::to_vec(&meta).ok()
        != serde_json::to_vec(
            &journal::read_meta(&run, epoch).map_err(|_| "source_revision_changed")?,
        )
        .ok()
    {
        return Err("source_revision_changed");
    }
    if materials_revision(&run, &meta).map_err(|_| "source_revision_changed")? != revision {
        return Err("source_revision_changed");
    }
    Ok((records, earlier, reasons))
}

// A manifest alone cannot pin a saved run: committed log files can be
// replaced independently. Blob content is also verified by its address hash.
fn materials_revision(run: &Directory, meta: &journal::Meta) -> io::Result<String> {
    use std::os::unix::fs::MetadataExt;
    let started = std::time::Instant::now();
    let mut hash = blake3::Hasher::new();
    hash.update(json!(meta).to_string().as_bytes());
    let mut add = |directory: &Directory, name: &str| -> io::Result<()> {
        if started.elapsed() > std::time::Duration::from_secs(1) {
            return Err(io::ErrorKind::TimedOut.into());
        }
        hash.update(name.as_bytes());
        match directory.open(name, false) {
            Ok(file) => {
                let m = file.metadata()?;
                hash.update(
                    format!(
                        "{:?}",
                        [
                            m.dev(),
                            m.ino(),
                            m.len(),
                            m.mtime() as u64,
                            m.mtime_nsec() as u64,
                            m.ctime() as u64,
                            m.ctime_nsec() as u64
                        ]
                    )
                    .as_bytes(),
                );
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                hash.update(b"missing");
            }
            Err(e) => return Err(e),
        }
        Ok(())
    };
    add(run, "meta.json")?;
    add(run, "project.json")?;
    let blobs = run.dir("blobs", false)?;
    for segment in &meta.segments {
        add(run, &format!("observations.{}.jsonl", segment.id))?;
        add(&blobs, &segment.base)?;
    }
    run.verify_location()?;
    Ok(hash.finalize().to_hex().to_string())
}
