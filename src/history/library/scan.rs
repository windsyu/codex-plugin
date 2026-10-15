use super::{model::*, native::NativeFile, presentation};
use crate::history::{
    files::BlobRoot,
    legacy_reader::{Collection, LegacyReader, Query, ReadBudget},
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;

pub(super) enum Step {
    More,
    Checkpoint(String),
    Document(Box<Document>),
    Done,
}
pub(super) struct Scan {
    pub source: Source,
    walker: Option<walkdir::IntoIter>,
    root: Option<BlobRoot>,
    native: Option<NativeFile>,
    legacy: Option<LegacyReader>,
    threads: VecDeque<crate::history::legacy_reader::Record>,
    cursor: Option<String>,
    started: bool,
    revision: Option<String>,
    current: Option<Document>,
    items_cursor: Option<String>,
    thread_key: Option<String>,
    bytes: usize,
    context_loaded: bool,
    preview_order: Option<u64>,
    pub issues: Vec<String>,
}
impl Scan {
    pub fn open(source: Source) -> Result<Self, &'static str> {
        let (root, walker, legacy) = match &source {
            Source::Native { codex_home, .. } => {
                let root = BlobRoot::open(codex_home).map_err(|_| "source_unavailable")?;
                let walker = walkdir::WalkDir::new(codex_home)
                    .min_depth(1)
                    .max_depth(12)
                    .max_open(8)
                    .follow_links(false)
                    .into_iter();
                (Some(root), Some(walker), None)
            }
            Source::Workbench { data_directory, .. } => {
                if !data_directory.join("runs").exists() {
                    (None, None, None)
                } else {
                    let root = BlobRoot::open(data_directory).map_err(|_| "source_unavailable")?;
                    (
                        Some(root),
                        Some(
                            walkdir::WalkDir::new(data_directory.join("runs"))
                                .min_depth(1)
                                .max_depth(1)
                                .max_open(2)
                                .follow_links(false)
                                .into_iter(),
                        ),
                        None,
                    )
                }
            }
            Source::Observer {
                id,
                database,
                blob_directory,
                ..
            } => (
                None,
                None,
                Some(
                    LegacyReader::open(
                        id,
                        database,
                        blob_directory.as_deref(),
                        &ReadBudget::default(),
                    )
                    .map_err(|e| e.code)?
                    .for_library(*blake3::hash(source.identity().as_bytes()).as_bytes()),
                ),
            ),
        };
        Ok(Self {
            source,
            walker,
            root,
            native: None,
            legacy,
            threads: VecDeque::new(),
            cursor: None,
            started: false,
            revision: None,
            current: None,
            items_cursor: None,
            thread_key: None,
            bytes: 0,
            context_loaded: false,
            preview_order: None,
            issues: vec![],
        })
    }
    pub fn skip_native(&mut self) {
        self.native = None;
    }
    pub fn step(&mut self, key: &[u8; 32]) -> Result<Step, &'static str> {
        if self.legacy.is_some() {
            return self.legacy_step(key);
        }
        if let Some(file) = &mut self.native {
            match file.step(&self.source, self.root.as_ref().unwrap(), key) {
                Ok(true) => {
                    return Ok(Step::Document(Box::new(
                        self.native.take().unwrap().document,
                    )));
                }
                Ok(false) => return Ok(Step::More),
                Err(_) => {
                    self.native = None;
                    self.issue("source_read_failed");
                    return Ok(Step::More);
                }
            }
        }
        for _ in 0..32 {
            let Some(walker) = &mut self.walker else {
                return Ok(Step::Done);
            };
            let Some(item) = walker.next() else {
                return Ok(Step::Done);
            };
            let item = match item {
                Ok(i) => i,
                Err(_) => {
                    self.issue("discovery_failed");
                    continue;
                }
            };
            match &self.source {
                Source::Native { codex_home, .. } => {
                    let relative = item
                        .path()
                        .strip_prefix(codex_home)
                        .map_err(|_| "invalid_source_path")?;
                    let first = relative.components().next().map(|c| c.as_os_str());
                    if !matches!(
                        first.and_then(|s| s.to_str()),
                        Some("sessions" | "archived_sessions")
                    ) {
                        if item.file_type().is_dir() {
                            walker.skip_current_dir();
                        }
                        continue;
                    }
                    if !item.file_type().is_file() {
                        if item.file_type().is_symlink() {
                            self.issue("unsafe_source_path");
                        }
                        continue;
                    }
                    let name = relative.to_string_lossy();
                    if !name.ends_with(".jsonl") && !name.ends_with(".jsonl.zst") {
                        continue;
                    }
                    match NativeFile::open(
                        self.root.as_ref().unwrap(),
                        name.into_owned(),
                        &self.source,
                    ) {
                        Ok(f) => self.native = Some(f),
                        Err(_) => self.issue("source_read_failed"),
                    };
                    return Ok(self
                        .native
                        .as_ref()
                        .map(|file| Step::Checkpoint(file.checkpoint()))
                        .unwrap_or(Step::More));
                }
                Source::Workbench { data_directory, .. } => {
                    if !item.file_type().is_dir() {
                        continue;
                    }
                    let Ok(epoch) = uuid::Uuid::parse_str(&item.file_name().to_string_lossy())
                    else {
                        continue;
                    };
                    let result = crate::workbench::recording::library::read(data_directory, epoch);
                    let (meta, records, project, issues, revision) = match result {
                        Ok(v) => v,
                        Err(_) => {
                            self.issue("run_read_failed");
                            continue;
                        }
                    };
                    let mut entry = Entry::new(&self.source, &epoch.to_string());
                    entry.run_id = Some(epoch.to_string());
                    entry.workspace_id = meta["workspaceId"].as_str().map(String::from);
                    entry.recorded_at = meta["startedAt"].as_str().map(String::from);
                    entry.source_revision = revision;
                    entry.coverage.state = "complete_for_source".into();
                    entry.capabilities.inspect_calls = true;
                    if let Some(p) = project {
                        entry.path(&p.cwd);
                        if entry.project_id.is_some() {
                            entry.project_basis = "workbench_explicit".into();
                        }
                    }
                    for issue in issues {
                        entry.issue(&issue);
                    }
                    if !meta["ended"].as_bool().unwrap_or(false) {
                        entry.issue("run_not_ended");
                    }
                    let mut bounded = vec![];
                    let mut bytes = 0;
                    let records = records.into_iter().flat_map(|mut record| {
                        if record["kind"] == "workbench_snapshot" {
                            let mut parts = vec![];
                            if let Some(raw) = record.get_mut("raw").and_then(Value::as_object_mut)
                            {
                                for field in [
                                    "items",
                                    "requests",
                                    "responses",
                                    "diagnostics",
                                    "toolContexts",
                                    "nativeCommands",
                                    "nativeFileChanges",
                                ] {
                                    if let Some(Value::Array(items)) = raw.remove(field) {
                                        parts.extend(
                                            items
                                                .into_iter()
                                                .map(|item| json!({"kind":field,"raw":item})),
                                        );
                                    }
                                }
                            }
                            parts.insert(0, record);
                            parts
                        } else {
                            vec![record]
                        }
                    });
                    for record in records {
                        let record = sanitize(&record, key);
                        presentation::observe(&mut entry, &record);
                        let size = record.to_string().len();
                        if bytes + size > 2 * 1024 * 1024 {
                            entry.issue("body_cache_limit");
                            break;
                        }
                        bytes += size;
                        bounded.push(record);
                    }
                    return Ok(Step::Document(Box::new(Document {
                        entry,
                        records: bounded,
                        locator: json!({"run":epoch}),
                    })));
                }
                Source::Observer { .. } => unreachable!(),
            }
        }
        Ok(Step::More)
    }
    pub fn issue(&mut self, code: &str) {
        if self.issues.len() < 32 && !self.issues.iter().any(|s| s == code) {
            self.issues.push(code.into());
        }
    }
    fn legacy_step(&mut self, key: &[u8; 32]) -> Result<Step, &'static str> {
        let reader = self.legacy.as_ref().unwrap();
        if let Some(doc) = &mut self.current {
            if !self.context_loaded {
                self.context_loaded = true;
                match reader.page(
                    &Query {
                        collection: Collection::Context,
                        scope: self.thread_key.clone(),
                    },
                    None,
                    32,
                    &ReadBudget::default(),
                ) {
                    Ok(page) => {
                        if self.revision.as_ref() != Some(&page.source_revision) {
                            return Err("source_changed");
                        }
                        for record in page.records {
                            let mut issues = doc.entry.coverage.reasons.clone();
                            issues.extend(record.issues.iter().cloned());
                            issues.extend(page.issues.iter().cloned());
                            let (kind, basis) = observer_conversation_kind(&record.fields, &issues);
                            let parent = record.fields["parentThreadId"].as_str();
                            let parent_entry = record.fields["parentThreadKey"]
                                .as_str()
                                .map(|id| Entry::new(&self.source, id).entry_id);
                            let parent_conflict = doc.entry.conversation_kind == "subagent"
                                && (doc.entry.parent_thread_id.as_deref() != parent
                                    || doc.entry.parent_entry_id != parent_entry);
                            // Context rows must not wash away an earlier unreliable identity.
                            if doc.entry.kind_basis != "metadata_invalid_v1"
                                && doc.entry.kind_basis != "metadata_conflict_v1"
                            {
                                if parent_conflict
                                    || (doc.entry.conversation_kind == "subagent"
                                        && kind != "subagent")
                                {
                                    doc.entry.conversation_kind = "unknown".into();
                                    doc.entry.kind_basis = "metadata_conflict_v1".into();
                                } else {
                                    doc.entry.conversation_kind = kind.into();
                                    doc.entry.kind_basis = basis.into();
                                }
                            }
                            doc.entry.parent_thread_id =
                                record.fields["parentThreadId"].as_str().map(String::from);
                            doc.entry.parent_entry_id = record.fields["parentThreadKey"]
                                .as_str()
                                .map(|id| Entry::new(&self.source, id).entry_id);
                            doc.entry.is_subagent = doc.entry.parent_thread_id.is_some()
                                || doc.entry.parent_entry_id.is_some();
                            doc.entry.agent_name = record.fields["agentNickname"]
                                .as_str()
                                .or(record.fields["agentRole"].as_str())
                                .map(String::from);
                            let value = json!({"kind":"observer_instructions","raw":record.fields,"issues":record.issues});
                            self.bytes += value.to_string().len();
                            doc.records.push(value);
                        }
                        for issue in page.issues {
                            if issue.contains("parentThread")
                                || issue.contains("identity")
                                || issue.contains("source_changed")
                                || issue.contains("source_revision_changed")
                            {
                                doc.entry.conversation_kind = "unknown".into();
                                doc.entry.kind_basis = "metadata_invalid_v1".into();
                            }
                            doc.entry.issue(&issue);
                        }
                    }
                    Err(error) => doc.entry.issue(error.code),
                }
                return Ok(Step::More);
            }
            let page = reader.page(
                &Query {
                    collection: Collection::Items,
                    scope: self.thread_key.clone(),
                },
                self.items_cursor.as_deref(),
                32,
                &ReadBudget::default(),
            );
            match page {
                Ok(page) => {
                    if self.revision.as_ref() != Some(&page.source_revision) {
                        return Err("source_changed");
                    }
                    for record in page.records {
                        let value = sanitize(
                            &json!({"kind":"observer_item","raw":record.fields,"issues":record.issues}),
                            key,
                        );
                        let previous_preview = doc.entry.preview.clone();
                        if presentation::observe(&mut doc.entry, &value) {
                            let order = value["raw"]["lastEventSeq"].as_u64();
                            if self.preview_order.is_some() && order < self.preview_order {
                                doc.entry.preview = previous_preview;
                            } else {
                                self.preview_order = order;
                            }
                        }
                        let size = value.to_string().len();
                        if self.bytes + size <= 2 * 1024 * 1024 {
                            self.bytes += size;
                            doc.records.push(value);
                        } else {
                            doc.entry.issue("body_cache_limit");
                        }
                    }
                    for issue in page.issues {
                        doc.entry.issue(&issue);
                    }
                    self.items_cursor = page.next_cursor;
                    if self.items_cursor.is_some() && self.bytes < 2 * 1024 * 1024 {
                        return Ok(Step::More);
                    }
                    if self.items_cursor.is_some() {
                        doc.entry.issue("body_cache_limit");
                    }
                }
                Err(e) => doc.entry.issue(e.code),
            }
            self.items_cursor = None;
            return Ok(Step::Document(Box::new(self.current.take().unwrap())));
        }
        if let Some(record) = self.threads.pop_front() {
            let fields = record.fields;
            let thread_key = fields["threadKey"]
                .as_str()
                .ok_or("thread_identity_missing")?
                .to_string();
            let mut entry = Entry::new(&self.source, &thread_key);
            entry.kind_basis = "observer_kind_unconfirmed_v1".into();
            entry.source_revision = self.revision.clone().unwrap_or_default();
            entry.title = fields["name"]
                .as_str()
                .and_then(|name| presentation::stored_title(name, key))
                .unwrap_or_else(|| "未命名会话".into());
            entry.native_thread_id = fields["codexThreadId"].as_str().map(String::from);
            entry.legacy_path(&fields, &record.issues);
            entry.recorded_at = fields["recencyAtMs"]
                .as_i64()
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|d| d.to_rfc3339());
            // Observer completeness concerns captured history, never the full native thread.
            entry.coverage.state = "partial".into();
            entry.issue("observer_capture_scope");
            entry.capabilities.inspect_calls = reader
                .manifest()
                .capabilities
                .iter()
                .any(|c| c.collection == Collection::Context && c.readable);
            self.current = Some(Document {
                entry,
                locator: json!({"thread":thread_key}),
                records: vec![
                    json!({"kind":"observer_context","raw":fields,"issues":record.issues}),
                ],
            });
            self.thread_key = Some(thread_key);
            self.bytes = 0;
            self.context_loaded = false;
            self.preview_order = None;
            return Ok(Step::More);
        }
        if self.started && self.cursor.is_none() {
            return Ok(Step::Done);
        }
        let page = reader
            .page(
                &Query {
                    collection: Collection::Threads,
                    scope: None,
                },
                self.cursor.as_deref(),
                32,
                &ReadBudget::default(),
            )
            .map_err(|e| e.code)?;
        if self
            .revision
            .as_ref()
            .is_some_and(|r| r != &page.source_revision)
        {
            return Err("source_changed");
        }
        self.revision = Some(page.source_revision);
        self.cursor = page.next_cursor;
        self.started = true;
        for issue in page.issues {
            self.issue(&issue);
        }
        self.threads = page.records.into_iter().collect();
        Ok(Step::More)
    }
}

/// Explicit preview only. Never loads legacy defaults, credentials, server settings or source code.
pub(super) fn preview(path: PathBuf) -> Result<Source, &'static str> {
    if !path.is_absolute() {
        return Err("absolute_config_path_required");
    }
    let parent = path.parent().ok_or("invalid_config_path")?;
    let root = BlobRoot::open(parent).map_err(|_| "config_unavailable")?;
    let file = root
        .read_file(
            path.file_name()
                .and_then(|s| s.to_str())
                .ok_or("invalid_config_path")?,
        )
        .map_err(|_| "config_unavailable")?;
    use std::io::Read;
    let mut bytes = vec![];
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "config_unavailable")?;
    if bytes.len() > 64 * 1024 {
        return Err("config_too_large");
    }
    let value: toml::Value =
        toml::from_str(std::str::from_utf8(&bytes).map_err(|_| "invalid_observer_config")?)
            .map_err(|_| "invalid_observer_config")?;
    let resolve = |s: &str| -> Result<PathBuf, &'static str> {
        if s.starts_with('~') {
            return Err("explicit_absolute_path_required");
        }
        crate::workbench::config::location_for_application(PathBuf::from(s).as_path(), parent)
            .map_err(|_| "invalid_source_path")
    };
    let database = resolve(
        value
            .get("storage")
            .and_then(|v| v.get("database"))
            .and_then(toml::Value::as_str)
            .ok_or("database_path_required")?,
    )?;
    let blob_directory = value
        .get("storage")
        .and_then(|v| v.get("blob_dir"))
        .and_then(toml::Value::as_str)
        .map(resolve)
        .transpose()?;
    // Multiple legacy native homes are ambiguous: do not invent a mapping.
    let native_home = value
        .get("sources")
        .and_then(toml::Value::as_array)
        .filter(|a| a.len() == 1)
        .and_then(|a| a[0].get("codex_home"))
        .and_then(toml::Value::as_str)
        .map(resolve)
        .transpose()?;
    Ok(Source::Observer {
        id: format!("observer-{}", &digest(&database.to_string_lossy())[..16]),
        database,
        blob_directory,
        native_home,
    })
}
