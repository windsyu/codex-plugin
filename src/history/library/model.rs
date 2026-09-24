use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryConfig {
    pub enabled: bool,
    #[serde(rename = "cacheLimitMiB")]
    pub cache_limit_mi_b: u32,
    pub sources: Vec<Source>,
}
impl Default for LibraryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_limit_mi_b: 512,
            sources: vec![],
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Source {
    Native {
        id: String,
        #[serde(rename = "codexHome")]
        codex_home: PathBuf,
    },
    Workbench {
        id: String,
        #[serde(rename = "dataDirectory")]
        data_directory: PathBuf,
    },
    Observer {
        id: String,
        database: PathBuf,
        #[serde(rename = "blobDirectory", default)]
        blob_directory: Option<PathBuf>,
        #[serde(rename = "nativeHome", default)]
        native_home: Option<PathBuf>,
    },
}
impl Source {
    pub fn id(&self) -> &str {
        match self {
            Self::Native { id, .. } | Self::Workbench { id, .. } | Self::Observer { id, .. } => id,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Native { .. } => "native",
            Self::Workbench { .. } => "workbench",
            Self::Observer { .. } => "observer",
        }
    }
    pub fn path(&self) -> &Path {
        match self {
            Self::Native { codex_home, .. } => codex_home,
            Self::Workbench { data_directory, .. } => data_directory,
            Self::Observer { database, .. } => database,
        }
    }
    pub fn identity(&self) -> String {
        digest(&serde_json::to_string(self).unwrap())
    }
}
pub fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex().to_string()
}
pub fn project(cwd: &str) -> Option<(String, String)> {
    let path = Path::new(cwd);
    if !path.is_absolute() || cwd.len() > 4096 || cwd.contains('\0') {
        return None;
    }
    let mut clean = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            _ => clean.push(c.as_os_str()),
        }
    }
    let path = clean
        .canonicalize()
        .unwrap_or(clean)
        .to_string_lossy()
        .into_owned();
    Some((format!("p_{}", digest(&path)), path))
}
impl LibraryConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(64..=2048).contains(&self.cache_limit_mi_b) || self.sources.len() > 8 {
            return Err("invalid_library_budget");
        }
        let mut ids = std::collections::HashSet::new();
        let mut paths = std::collections::HashSet::new();
        for s in &self.sources {
            if s.id().is_empty()
                || s.id().len() > 64
                || s.id().starts_with("default-")
                || !s
                    .id()
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                || !ids.insert(s.id())
            {
                return Err("invalid_source_id");
            }
            let valid = |p: &Path| {
                p.is_absolute()
                    && p.as_os_str().len() <= 4096
                    && !p.as_os_str().as_encoded_bytes().contains(&0)
                    && !p
                        .components()
                        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            };
            if !valid(s.path()) || !paths.insert((s.kind(), s.path())) {
                return Err("invalid_source_path");
            }
            if let Source::Observer {
                blob_directory,
                native_home,
                ..
            } = s
                && (blob_directory.as_ref().is_some_and(|p| !valid(p))
                    || native_home.as_ref().is_some_and(|p| !valid(p)))
            {
                return Err("invalid_source_path");
            }
        }
        Ok(())
    }
    pub fn resolved(&self, home: &Path, data: &Path) -> Result<Vec<Source>, &'static str> {
        self.validate()?;
        if !self.enabled {
            return Ok(vec![]);
        }
        let mut all = vec![
            Source::Native {
                id: "default-native".into(),
                codex_home: home.into(),
            },
            Source::Workbench {
                id: "default-workbench".into(),
                data_directory: data.into(),
            },
        ];
        let cache = data.join("library");
        for source in &self.sources {
            let path = source
                .path()
                .canonicalize()
                .unwrap_or_else(|_| source.path().into());
            // The automatic workbench root is allowed; explicit sources cannot contain the derived cache.
            if path.starts_with(&cache)
                || cache.starts_with(&path)
                || all.iter().any(|s| {
                    s.kind() == source.kind()
                        && s.path().canonicalize().unwrap_or_else(|_| s.path().into()) == path
                })
            {
                return Err("overlapping_history_source");
            }
            all.push(source.clone());
        }
        Ok(all)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub state: String,
    pub reasons: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub read: bool,
    pub resume: bool,
    pub inspect_calls: bool,
    pub manage: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub entry_id: String,
    pub source_id: String,
    #[serde(default)]
    pub source_identity: String,
    pub kind: String,
    pub source_revision: String,
    pub project_id: Option<String>,
    pub project_path: Option<String>,
    #[serde(default)]
    pub recorded_cwd: Option<String>,
    #[serde(default)]
    pub project_basis: String,
    pub title: String,
    pub recorded_at: Option<String>,
    pub native_thread_id: Option<String>,
    pub run_id: Option<String>,
    pub coverage: Coverage,
    #[serde(default)]
    pub diagnostics: Vec<Value>,
    pub capabilities: Capabilities,
    pub related_entry_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub parent_thread_id: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub parent_entry_id: Option<String>,
    #[serde(default)]
    pub is_subagent: bool,
}
impl Entry {
    pub fn new(source: &Source, key: &str) -> Self {
        Self {
            entry_id: format!("e_{}", digest(&format!("{}:{key}", source.identity()))),
            source_id: source.id().into(),
            source_identity: source.identity(),
            kind: source.kind().into(),
            source_revision: String::new(),
            project_id: None,
            project_path: None,
            recorded_cwd: None,
            project_basis: "unknown".into(),
            title: "未命名会话".into(),
            recorded_at: None,
            native_thread_id: None,
            run_id: None,
            coverage: Coverage {
                state: "unknown".into(),
                reasons: vec![],
            },
            diagnostics: vec![],
            capabilities: Capabilities {
                read: true,
                resume: false,
                inspect_calls: false,
                manage: false,
            },
            related_entry_ids: vec![],
            workspace_id: None,
            parent_thread_id: None,
            agent_name: None,
            parent_entry_id: None,
            is_subagent: false,
        }
    }
    pub fn record_cwd(&mut self, cwd: Option<&str>) {
        self.recorded_cwd = cwd
            .filter(|s| !s.is_empty() && s.len() <= 4096 && !s.contains('\0'))
            .map(String::from);
        if self.recorded_cwd.is_none() {
            self.issue("recorded_cwd_unavailable");
        }
    }
    pub fn path(&mut self, cwd: &str) {
        self.record_cwd(Some(cwd));
        self.project_id = None;
        self.project_path = None;
        self.project_basis = "unknown".into();
        if let Some((id, path)) = project(cwd) {
            self.project_id = Some(id);
            self.project_path = Some(path);
            self.project_basis = "cwd_inferred".into();
        } else {
            self.issue("project_cwd_invalid");
        }
    }
    pub fn native_path(&mut self, cwd: Option<&str>, originator: Option<&str>) {
        let Some(cwd) = cwd else {
            self.record_cwd(None);
            return;
        };
        self.record_cwd(Some(cwd));
        if self.recorded_cwd.is_some() && desktop_generated(cwd, originator) {
            self.project_id = None;
            self.project_path = None;
            self.project_basis = "desktop_generated".into();
        } else {
            self.path(cwd);
        }
    }
    pub fn legacy_path(&mut self, fields: &Value, issues: &[String]) {
        self.record_cwd(fields["cwd"].as_str());
        for issue in issues {
            self.issue(issue);
        }
        self.project_id = None;
        self.project_path = None;
        self.project_basis = "unknown".into();
        if issues.iter().any(|s| s.ends_with(":projectKey")) {
            self.issue("legacy_project_unavailable");
            return;
        }
        match fields.get("projectKey") {
            Some(Value::Null) => self.project_basis = "legacy_recorded".into(),
            Some(Value::String(key)) if key.trim().is_empty() => {
                self.project_basis = "legacy_recorded".into()
            }
            Some(Value::String(key)) => {
                if let Some((id, path)) = project(key) {
                    self.project_id = Some(id);
                    self.project_path = Some(path);
                    self.project_basis = "legacy_recorded".into();
                } else {
                    self.issue("legacy_project_invalid");
                }
            }
            _ => self.issue("legacy_project_unavailable"),
        }
    }
    pub fn issue_at(&mut self, code: &str, byte_offset: u64) {
        self.issue(code);
        if self.diagnostics.len() < 32 {
            self.diagnostics
                .push(serde_json::json!({"code":code,"byteOffset":byte_offset}));
        }
    }
    pub fn issue(&mut self, code: &str) {
        self.coverage.state = "partial".into();
        if self.coverage.reasons.len() < 32 && !self.coverage.reasons.iter().any(|r| r == code) {
            self.coverage.reasons.push(code.into());
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub id: String,
    pub identity: String,
    pub kind: String,
    pub state: String,
    pub message: String,
    pub indexed_entries: u64,
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Query {
    pub project_id: Option<String>,
    pub source_id: Option<String>,
    pub kind: Option<String>,
    pub group: Option<String>,
    pub window: bool,
    pub record: Option<u64>,
    pub q: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
    pub source_revision: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub revision: String,
    pub records: Vec<Value>,
    pub next_cursor: Option<String>,
    pub coverage: Coverage,
}
pub(crate) struct Document {
    pub entry: Entry,
    pub records: Vec<Value>,
    pub locator: Value,
}

/// Structured redaction plus the workbench's pure inline credential masking.
/// Never loads native authentication/configuration in order to redact history.
pub(crate) fn sanitize(raw: &Value, key: &[u8; 32]) -> Value {
    fn text(value: &mut Value, name: &str) {
        match value {
            Value::Object(map) => {
                for (name, value) in map {
                    text(value, name);
                }
            }
            Value::Array(items) => {
                for value in items {
                    text(value, name);
                }
            }
            Value::String(value)
                if !matches!(name, "cwd" | "path" | "id" | "type" | "role" | "timestamp")
                    && !name.ends_with("_id") =>
            {
                *value = safe_text(value)
            }
            _ => {}
        }
    }
    let (mut safe, _) = crate::history::redact::redact(raw, key);
    text(&mut safe, "");
    safe
}
pub(crate) fn safe_text(value: &str) -> String {
    static POLICY: std::sync::OnceLock<
        std::sync::Arc<crate::workbench::redaction::RedactionPolicy>,
    > = std::sync::OnceLock::new();
    POLICY
        .get_or_init(|| crate::workbench::redaction::RedactionPolicy::new(vec![]).unwrap())
        .scrub(value)
        .as_str()
        .into()
}

// Pure compatibility heuristic: an execution directory is not itself project intent.
// Accept both platform separators; do not inspect repositories or session content.
fn desktop_generated(cwd: &str, originator: Option<&str>) -> bool {
    if !originator.is_some_and(|s| {
        s.eq_ignore_ascii_case("Codex Desktop") || s.eq_ignore_ascii_case("codex_work_desktop")
    }) {
        return false;
    }
    let normalized = cwd.replace('\\', "/");
    let absolute = normalized.starts_with('/') || normalized.as_bytes().get(1..3) == Some(b":/");
    if !absolute {
        return false;
    }
    let mut segments = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(part),
        }
    }
    if segments.len() < 4 {
        return false;
    }
    let tail = &segments[segments.len() - 4..];
    let date = tail[2].as_bytes();
    tail[0] == "Documents"
        && tail[1] == "Codex"
        && date.len() == 10
        && date[4] == b'-'
        && date[7] == b'-'
        && date
            .iter()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit())
        && chrono::NaiveDate::parse_from_str(tail[2], "%Y-%m-%d").is_ok()
}
