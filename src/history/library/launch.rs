//! Read-only launch identity resolution. Never returns source locators over HTTP.
use super::*;
use crate::history::files::{BlobRoot, signature};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Selection {
    pub path: PathBuf,
    pub resume: Option<NativeResume>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct NativeResume {
    pub home: PathBuf,
    pub id: uuid::Uuid,
    path: String,
    signature: [u64; 7],
    cwd: PathBuf,
}
impl NativeResume {
    /// Explicit command entry has no catalog dependency (including cold startup).
    /// The same source/identity checks apply before any version probe or PTY.
    pub fn direct(home: &std::path::Path, id: uuid::Uuid, cwd: &std::path::Path) -> Result<Self> {
        let path = unique_rollout(home, id)?;
        let root = BlobRoot::open(home).map_err(|_| "resume_source_unavailable")?;
        let file = root
            .read_file(&path)
            .map_err(|_| "resume_source_unavailable")?;
        let native = Self {
            home: home.to_owned(),
            id,
            path,
            cwd: cwd.to_owned(),
            signature: signature(&file.metadata().map_err(|_| "resume_source_unavailable")?),
        };
        native.verify()?;
        Ok(native)
    }
    pub fn verify(&self) -> Result<()> {
        let root = BlobRoot::open(&self.home).map_err(|_| "resume_source_unavailable")?;
        let file = root
            .read_file(&self.path)
            .map_err(|_| "resume_source_unavailable")?;
        if signature(&file.metadata().map_err(|_| "resume_source_unavailable")?) != self.signature {
            return Err("source_revision_changed");
        }
        let input = file.try_clone().map_err(|_| "resume_source_unavailable")?;
        let reader: Box<dyn Read> = if self.path.ends_with(".zst") {
            let mut decoder =
                zstd::stream::read::Decoder::new(input).map_err(|_| "resume_unavailable")?;
            decoder
                .window_log_max(23)
                .map_err(|_| "resume_unavailable")?;
            Box::new(decoder)
        } else {
            Box::new(input)
        };
        let mut line = Vec::new();
        BufReader::new(reader.take(1024 * 1024 + 1))
            .read_until(b'\n', &mut line)
            .map_err(|_| "resume_unavailable")?;
        if line.len() > 1024 * 1024 || !line.ends_with(b"\n") {
            return Err("resume_unavailable");
        }
        let raw: Value = serde_json::from_slice(&line).map_err(|_| "resume_unavailable")?;
        let payload = &raw["payload"];
        let id = payload["id"]
            .as_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok());
        let cwd = payload["cwd"]
            .as_str()
            .filter(|p| std::path::Path::new(p).is_absolute())
            .and_then(|p| std::fs::canonicalize(p).ok());
        if raw["type"] != "session_meta" || id != Some(self.id) || cwd.as_ref() != Some(&self.cwd) {
            return Err("resume_identity_changed");
        }
        if !payload["history_base"].is_null() {
            return Err("resume_history_unsupported");
        }
        if signature(&file.metadata().map_err(|_| "resume_source_unavailable")?) != self.signature
            || signature(
                &root
                    .read_file(&self.path)
                    .map_err(|_| "resume_source_unavailable")?
                    .metadata()
                    .map_err(|_| "resume_source_unavailable")?,
            ) != self.signature
        {
            return Err("source_revision_changed");
        }
        // The native CLI accepts an ID, not a pinned rollout path. Ambiguous or
        // noncanonical filenames are readable but cannot be safely launched here.
        if unique_rollout(&self.home, self.id)? != self.path {
            return Err("resume_lookup_unsupported");
        }
        Ok(())
    }
}

fn filename_id(name: &str) -> Option<uuid::Uuid> {
    let name = name.strip_suffix(".zst").unwrap_or(name);
    let core = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    chrono::NaiveDateTime::parse_from_str(core.get(..19)?, "%Y-%m-%dT%H-%M-%S").ok()?;
    if core.get(19..20)? != "-" {
        return None;
    }
    let ids = core.get(20..)?;
    let (thread, rollout) = ids.split_once('_').unwrap_or((ids, ids));
    uuid::Uuid::parse_str(rollout).ok()?;
    uuid::Uuid::parse_str(thread).ok().filter(|id| !id.is_nil())
}

/// Bounded metadata-only lookup on the Run/history worker, never on HTTP or
/// model forwarding executors. No native SQLite access and no history writes.
fn unique_rollout(home: &std::path::Path, id: uuid::Uuid) -> Result<String> {
    let started = Instant::now();
    let mut count = 0;
    let mut found = None;
    for directory in ["sessions", "archived_sessions"] {
        let root = home.join(directory);
        if !root.try_exists().map_err(|_| "resume_source_unavailable")? {
            continue;
        }
        for item in walkdir::WalkDir::new(&root).follow_links(false).max_open(8) {
            count += 1;
            if count > 100_000 || started.elapsed() > Duration::from_secs(2) {
                return Err("resume_lookup_limit");
            }
            let item = item.map_err(|_| "resume_source_unavailable")?;
            if item.file_type().is_symlink() || item.depth() > 12 {
                return Err("resume_lookup_unsupported");
            }
            if filename_id(&item.file_name().to_string_lossy()) != Some(id) {
                continue;
            }
            if !item.file_type().is_file() || found.is_some() {
                return Err("resume_lookup_unsupported");
            }
            found = Some(
                item.path()
                    .strip_prefix(home)
                    .map_err(|_| "resume_source_unavailable")?
                    .to_str()
                    .ok_or("resume_lookup_unsupported")?
                    .to_owned(),
            );
        }
    }
    found.ok_or("resume_lookup_unsupported")
}

pub(super) fn resolve(
    shared: &Shared,
    sources: &[Source],
    project: Option<String>,
    resume: Option<(String, String)>,
) -> Result<Selection> {
    let db = read_db(shared)?.ok_or("project_unavailable")?;
    let identities =
        serde_json::to_string(&sources.iter().map(Source::identity).collect::<Vec<_>>()).unwrap();
    let result = if let Some((id, revision)) = resume {
        let found: Option<(String,String)> = db.query_row("SELECT e.metadata,l.locator FROM catalog_entries e JOIN catalog_sources s ON e.generation=s.generation JOIN catalog_locators l ON l.generation=e.generation AND l.entry=e.id WHERE e.id=?1 AND s.identity IN(SELECT value FROM json_each(?2))", params![id,identities], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_| "cache_unavailable")?;
        let (metadata, locator) = found.ok_or("entry_unavailable")?;
        let entry: Entry = serde_json::from_str(&metadata).map_err(|_| "cache_invalid")?;
        if entry.source_revision != revision {
            return Err("source_revision_changed");
        }
        if project.is_some() && project != entry.project_id {
            return Err("project_changed");
        }
        if entry.kind != "native"
            || !entry.capabilities.resume
            || entry.coverage.reasons.iter().any(|r| {
                matches!(
                    r.as_str(),
                    "conflicting_session_identity"
                        | "inherited_history_not_loaded"
                        | "source_changed_during_read"
                )
            })
        {
            return Err("resume_unavailable");
        }
        let home = sources
            .iter()
            .find_map(|s| match s {
                Source::Native { id, codex_home } if id == &entry.source_id => {
                    Some(codex_home.clone())
                }
                _ => None,
            })
            .ok_or("source_revoked")?;
        let recorded_cwd = PathBuf::from(entry.recorded_cwd.ok_or("project_unavailable")?);
        if !recorded_cwd.is_absolute() {
            return Err("project_unavailable");
        }
        let path = recorded_cwd
            .canonicalize()
            .map_err(|_| "project_unavailable")?;
        let locator: Value = serde_json::from_str(&locator).map_err(|_| "cache_invalid")?;
        let duplicates: i64 = db.query_row(
            "SELECT count(*) FROM catalog_entries WHERE generation IN(SELECT generation FROM catalog_sources WHERE identity IN(SELECT value FROM json_each(?1))) AND json_extract(metadata,'$.sourceId')=?2 AND json_extract(metadata,'$.nativeThreadId')=?3",
            params![identities, entry.source_id, entry.native_thread_id], |r| r.get(0)
        ).map_err(|_| "cache_unavailable")?;
        if duplicates != 1 {
            return Err("resume_lookup_unsupported");
        }
        let native = NativeResume {
            home,
            cwd: path.clone(),
            id: entry
                .native_thread_id
                .as_deref()
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                .filter(|id| !id.is_nil())
                .ok_or("resume_unavailable")?,
            path: locator["path"].as_str().ok_or("resume_unavailable")?.into(),
            signature: serde_json::from_value(locator["signature"].clone())
                .map_err(|_| "resume_unavailable")?,
        };
        drop(db);
        native.verify()?;
        Selection {
            path,
            resume: Some(native),
        }
    } else {
        let project = project.ok_or("project_required")?;
        let path: Option<String> = db.query_row("SELECT json_extract(e.metadata,'$.projectPath') FROM catalog_entries e JOIN catalog_sources s ON e.generation=s.generation WHERE e.project=?1 AND s.identity IN(SELECT value FROM json_each(?2)) LIMIT 1", params![project, identities], |r| r.get(0)).optional().map_err(|_| "cache_unavailable")?.flatten();
        Selection {
            path: path.ok_or("project_unavailable")?.into(),
            resume: None,
        }
    };
    let (_, current) = shared.policy()?;
    if current != sources {
        return Err("source_revoked");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative_recorded_cwd_is_readable_but_cannot_resume() {
        for cwd in [".", "../."] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join("native");
            std::fs::create_dir_all(home.join("sessions")).unwrap();
            let id = uuid::Uuid::new_v4();
            let file = home.join(format!("sessions/rollout-2026-09-23T00-00-00-{id}.jsonl"));
            std::fs::write(
                &file,
                format!(
                    "{}\n",
                    json!({"type":"session_meta","payload":{"id":id,"cwd":cwd}})
                ),
            )
            .unwrap();
            let library = HistoryLibrary::with_policy(
                home.clone(),
                temp.path().join("history"),
                Arc::new(|| Ok(LibraryConfig::default())),
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(8);
            let entry = loop {
                if let Ok(result) = query(
                    &library.handle.shared,
                    Operation::List(Query::default(), false),
                ) && let Some(entry) = result["records"].as_array().and_then(|a| a.first())
                {
                    break serde_json::from_value::<Entry>(entry.clone()).unwrap();
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(20));
            };
            assert_eq!(entry.recorded_cwd.as_deref(), Some(cwd));
            assert!(entry.capabilities.read);
            let (_, sources) = library.handle.shared.policy().unwrap();
            assert_eq!(
                resolve(
                    &library.handle.shared,
                    &sources,
                    None,
                    Some((entry.entry_id, entry.source_revision))
                )
                .unwrap_err(),
                "project_unavailable"
            );
            // Metadata revalidation must reject a relative source cwd even if a
            // caller supplies its accidentally matching canonical server path.
            if let Ok(accidental) = std::fs::canonicalize(cwd) {
                assert_eq!(
                    NativeResume::direct(&home, id, &accidental).unwrap_err(),
                    "resume_identity_changed"
                );
            }
        }
    }
    #[test]
    fn unassigned_resume_uses_recorded_working_directory() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("native");
        let cwd = temp.path().join("Documents/Codex/2026-09-23/chat");
        std::fs::create_dir_all(home.join("sessions")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let id = uuid::Uuid::new_v4();
        let file = home.join(format!("sessions/rollout-2026-09-23T00-00-00-{id}.jsonl"));
        std::fs::write(&file, format!("{}\n", json!({"type":"session_meta","payload":{"id":id,"cwd":cwd,"originator":"Codex Desktop"}}))).unwrap();
        let library = HistoryLibrary::with_policy(
            home,
            temp.path().join("history"),
            Arc::new(|| Ok(LibraryConfig::default())),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let entry = loop {
            if let Ok(result) = query(
                &library.handle.shared,
                Operation::List(Query::default(), false),
            ) && let Some(entry) = result["records"].as_array().and_then(|a| a.first())
            {
                break serde_json::from_value::<Entry>(entry.clone()).unwrap();
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(entry.project_id.is_none());
        let (_, sources) = library.handle.shared.policy().unwrap();
        let selection = resolve(
            &library.handle.shared,
            &sources,
            None,
            Some((entry.entry_id.clone(), entry.source_revision.clone())),
        )
        .unwrap();
        assert_eq!(selection.path, cwd.canonicalize().unwrap());
        selection.resume.unwrap().verify().unwrap();
        std::fs::remove_dir(&cwd).unwrap();
        assert!(
            resolve(
                &library.handle.shared,
                &sources,
                None,
                Some((entry.entry_id, entry.source_revision))
            )
            .is_err()
        );
    }
    #[test]
    fn resume_checks_metadata_filename_uniqueness_and_inherited_history() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let dir = home.join("sessions/2026/09/22");
        std::fs::create_dir_all(&dir).unwrap();
        let id = uuid::Uuid::new_v4();
        let path = dir.join(format!("rollout-2026-09-22T00-00-00-{id}.jsonl"));
        let raw = |id, history: Value| {
            format!(
                "{}\n",
                json!({"type":"session_meta","payload":{"id":id,"cwd":home.canonicalize().unwrap(),"history_base":history}})
            )
        };
        std::fs::write(&path, raw(id, Value::Null)).unwrap();
        let native = NativeResume::direct(home, id, &home.canonicalize().unwrap()).unwrap();
        let other = dir.join(format!("rollout-2026-09-22T00-00-01-{id}.jsonl"));
        std::fs::copy(&path, &other).unwrap();
        assert_eq!(native.verify(), Err("resume_lookup_unsupported"));
        std::fs::remove_file(&other).unwrap();
        std::fs::write(&path, raw(uuid::Uuid::new_v4(), Value::Null)).unwrap();
        assert_eq!(
            NativeResume::direct(home, id, &home.canonicalize().unwrap()).unwrap_err(),
            "resume_identity_changed"
        );
        std::fs::write(&path, raw(id, json!({"thread_id":uuid::Uuid::new_v4()}))).unwrap();
        assert_eq!(
            NativeResume::direct(home, id, &home.canonicalize().unwrap()).unwrap_err(),
            "resume_history_unsupported"
        );
        std::fs::write(&path, raw(id, Value::Null)).unwrap();
        std::fs::rename(&path, dir.join("renamed.jsonl")).unwrap();
        assert_eq!(
            NativeResume::direct(home, id, &home.canonicalize().unwrap()).unwrap_err(),
            "resume_lookup_unsupported"
        );
    }
}
