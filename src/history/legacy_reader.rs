//! Bounded, synchronous legacy queries for a background history worker.
//! Never call directly on the async HTTP executor. Source content remains
//! separate from native/workbench records; unknown coverage is not completed.

use super::{
    files,
    legacy_contract::{CONTRACTS, contract, field_name},
    readonly_vfs,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, config::DbConfig, limits::Limit, params,
    types::ValueRef,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const FIELD_BYTES: usize = 256 * 1024;
const PAGE_BYTES: usize = 2 * 1024 * 1024;
const BLOB_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Collection {
    Projects,
    Threads,
    Context,
    Relations,
    Turns,
    Items,
    Sources,
    Epochs,
    Gaps,
    Raw,
    Blobs,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub collection: Collection,
    pub readable: bool,
    pub reasons: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub contract_version: u32,
    pub schema_version: i64,
    pub validated_schema: bool,
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Query {
    pub collection: Collection,
    /// Thread key for Context/Relations/Turns/Items/Raw, project key for
    /// Threads, source ID for Epochs/Gaps. None means all for lists only.
    pub scope: Option<String>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub fields: Value,
    pub issues: Vec<String>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub source_id: String,
    pub source_revision: String,
    pub records: Vec<Record>,
    pub next_cursor: Option<String>,
    pub issues: Vec<String>,
}
/// Diagnostics contain only assigned source identity, operation and code.
/// SQLite/OS errors can embed private SQL, paths or payloads and are not exposed.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fault {
    pub source_id: String,
    pub operation: &'static str,
    pub code: &'static str,
}
impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "history {} {}: {}",
            self.source_id, self.operation, self.code
        )
    }
}
impl std::error::Error for Fault {}
pub type Result<T> = std::result::Result<T, Fault>;

#[derive(Clone)]
pub struct ReadBudget {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}
impl Default for ReadBudget {
    fn default() -> Self {
        Self::new(Duration::from_secs(2))
    }
}
impl ReadBudget {
    pub fn new(timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + timeout.min(Duration::from_secs(2)),
            cancelled: Arc::default(),
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
    fn stopped(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed) || Instant::now() >= self.deadline
    }
    fn code(&self) -> &'static str {
        if self.cancelled.load(Ordering::Relaxed) {
            "cancelled"
        } else {
            "query_timeout"
        }
    }
}

pub struct LegacyReader {
    connection: Connection,
    source_id: String,
    path: PathBuf,
    identity: [u64; 2],
    schema_cookie: i64,
    blobs: Option<files::BlobRoot>,
    key: [u8; 32],
    manifest: Manifest,
    library_revision: bool,
}
impl LegacyReader {
    pub(crate) fn for_library(mut self, key: [u8; 32]) -> Self {
        self.key = key;
        self.library_revision = true;
        self
    }
    pub fn open(
        source_id: &str,
        path: &Path,
        blob_dir: Option<&Path>,
        budget: &ReadBudget,
    ) -> Result<Self> {
        let fault = |code| Fault {
            source_id: source_id.to_owned(),
            operation: "open",
            code,
        };
        if source_id.is_empty()
            || source_id.len() > 128
            || !source_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(Fault {
                source_id: "invalid".into(),
                operation: "open",
                code: "invalid_source_id",
            });
        }
        if budget.stopped() {
            return Err(fault(budget.code()));
        }
        // Refuse final symlinks before canonicalizing; registered parent paths
        // may be aliases (e.g. macOS /var). Revalidate identity every query.
        files::regular(path).map_err(|_| fault("source_unavailable"))?;
        let path = path
            .canonicalize()
            .map_err(|_| fault("source_unavailable"))?;
        let before = files::source_signature(&path).map_err(|_| fault("unsafe_source_path"))?;
        readonly_vfs::register().map_err(|_| fault("readonly_vfs_unavailable"))?;
        let mut uri =
            reqwest::Url::from_file_path(&path).map_err(|_| fault("unsafe_source_path"))?;
        uri.set_query(Some("mode=ro&readonly_shm=1"));
        let connection = Connection::open_with_flags_and_vfs(
            uri.as_str(),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            readonly_vfs::NAME,
        )
        .map_err(|_| fault("source_unavailable"))?;
        let progress = budget.clone();
        connection.progress_handler(1000, Some(move || progress.stopped()));
        connection
            .busy_timeout(Duration::from_millis(80))
            .map_err(|_| fault("source_unavailable"))?;
        for (config, enabled) in [
            (DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true),
            (DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false),
            (DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false),
            (DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false),
            (DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true),
        ] {
            connection
                .set_db_config(config, enabled)
                .map_err(|_| fault("readonly_setup_failed"))?;
        }
        connection
            .set_limit(Limit::SQLITE_LIMIT_LENGTH, 8 * 1024 * 1024)
            .map_err(|_| fault("readonly_setup_failed"))?;
        connection
            .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 64 * 1024)
            .map_err(|_| fault("readonly_setup_failed"))?;
        connection
            .set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)
            .map_err(|_| fault("readonly_setup_failed"))?;
        // Extensions are unavailable (load_extension Cargo feature is absent).
        connection.execute_batch("PRAGMA query_only=ON; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-2048; PRAGMA mmap_size=0;")
            .map_err(|error| fault(sql_code(&error, budget)))?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| fault(sql_code(&error, budget)))?;
        let mut manifest = probe(&transaction).map_err(|error| fault(sql_code(&error, budget)))?;
        let schema_cookie = transaction
            .query_row("PRAGMA schema_version", [], |r| r.get(0))
            .map_err(|error| fault(sql_code(&error, budget)))?;
        transaction
            .rollback()
            .map_err(|error| fault(sql_code(&error, budget)))?;
        let after = files::source_signature(&path).map_err(|_| fault("source_changed"))?;
        if before != after {
            return Err(fault("source_changed"));
        }
        let blobs = blob_dir.and_then(|path| files::BlobRoot::open(path).ok());
        if blobs.is_none() {
            for capability in &mut manifest.capabilities {
                if capability.collection == Collection::Blobs {
                    capability.readable = false;
                    capability.reasons.push("blob_root_unavailable".into());
                }
            }
        }
        Ok(Self {
            connection,
            source_id: source_id.into(),
            path,
            identity: [before[0][0], before[0][1]],
            schema_cookie,
            blobs,
            key: rand::random(),
            library_revision: false,
            manifest,
        })
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn fault(&self, operation: &'static str, code: &'static str) -> Fault {
        Fault {
            source_id: self.source_id.clone(),
            operation,
            code,
        }
    }
    fn begin(&self, budget: &ReadBudget) -> Result<rusqlite::Transaction<'_>> {
        if budget.stopped() {
            return Err(self.fault("read", budget.code()));
        }
        let progress = budget.clone();
        self.connection
            .progress_handler(1000, Some(move || progress.stopped()));
        let signature = files::source_signature(&self.path)
            .map_err(|_| self.fault("read", "source_changed"))?;
        if signature[0][..2] != self.identity {
            return Err(self.fault("read", "source_replaced"));
        }
        self.connection
            .unchecked_transaction()
            .map_err(|error| self.fault("read", sql_code(&error, budget)))
    }
    fn revision(&self, connection: &Connection, budget: &ReadBudget) -> Result<String> {
        // Force a SQLite snapshot before fingerprinting files. Both ends of the
        // query must match; concurrent changes cause retry, never mixed pages.
        let cookie: i64 = connection
            .query_row("PRAGMA schema_version", [], |row| row.get(0))
            .map_err(|error| self.fault("read", sql_code(&error, budget)))?;
        if cookie != self.schema_cookie {
            return Err(self.fault("read", "schema_changed"));
        }
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| self.fault("read", sql_code(&error, budget)))?;
        if version != self.manifest.schema_version {
            return Err(self.fault("read", "schema_changed"));
        }
        let data_version: i64 = connection
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .map_err(|error| self.fault("read", sql_code(&error, budget)))?;
        let signature = files::source_signature(&self.path)
            .map_err(|_| self.fault("read", "source_changed"))?;
        if signature[0][..2] != self.identity {
            return Err(self.fault("read", "source_replaced"));
        }
        let bytes = serde_json::to_vec(&(
            signature,
            if self.library_revision {
                0
            } else {
                data_version
            },
        ))
        .expect("metadata JSON");
        Ok(blake3::keyed_hash(&self.key, &bytes).to_hex().to_string())
    }
    pub fn page(
        &self,
        query: &Query,
        cursor: Option<&str>,
        limit: usize,
        budget: &ReadBudget,
    ) -> Result<Page> {
        if !(1..=100).contains(&limit)
            || query
                .scope
                .as_ref()
                .is_some_and(|s| s.len() > 4096 || s.contains('\0'))
            || query.collection == Collection::Blobs
        {
            return Err(self.fault("page", "invalid_query"));
        }
        if matches!(
            query.collection,
            Collection::Context
                | Collection::Relations
                | Collection::Turns
                | Collection::Items
                | Collection::Raw
        ) && query.scope.is_none()
        {
            return Err(self.fault("page", "scope_required"));
        }
        self.require(query.collection)?;
        let tx = self.begin(budget)?;
        let revision = self.revision(&tx, budget)?;
        let binding = blake3::keyed_hash(
            &self.key,
            &serde_json::to_vec(&(query, &revision)).expect("query JSON"),
        )
        .to_hex()
        .to_string();
        let after = cursor
            .map(|cursor| self.decode_cursor(cursor, &binding))
            .transpose()?;
        let spec = contract(query.collection);
        let columns: Vec<_> = if query.collection == Collection::Projects {
            "project_key cwd thread_count"
        } else {
            spec.columns
        }
        .split_whitespace()
        .collect();
        let select = spec.columns.split_whitespace().map(|c| format!("CASE WHEN length(CAST({c} AS BLOB))<={FIELD_BYTES} THEN {c} END,coalesce(length(CAST({c} AS BLOB))>{FIELD_BYTES},0)")).collect::<Vec<_>>().join(",");
        let predicate = match query.collection {
            Collection::Threads => "(?2 IS NULL OR project_key=?2)",
            Collection::Context | Collection::Turns | Collection::Items | Collection::Raw => {
                "thread_key=?2"
            }
            Collection::Relations => {
                "(thread_key=?2 OR parent_thread_key=?2 OR forked_from_thread_key=?2 OR thread_key IN (SELECT parent_thread_key FROM threads WHERE thread_key=?2 UNION SELECT forked_from_thread_key FROM threads WHERE thread_key=?2))"
            }
            Collection::Epochs | Collection::Gaps => "(?2 IS NULL OR source_id=?2)",
            _ => "?2 IS NULL",
        };
        let sql = if query.collection == Collection::Projects {
            format!(
                "SELECT min(rowid),CASE WHEN length(CAST(project_key AS BLOB))<={FIELD_BYTES} THEN project_key END,coalesce(length(CAST(project_key AS BLOB))>{FIELD_BYTES},0),CASE WHEN length(CAST(min(cwd) AS BLOB))<={FIELD_BYTES} THEN min(cwd) END,coalesce(length(CAST(min(cwd) AS BLOB))>{FIELD_BYTES},0),count(*),0 FROM threads WHERE ?2 IS NULL GROUP BY project_key HAVING (?1 IS NULL OR min(rowid)>?1) ORDER BY min(rowid) LIMIT ?3"
            )
        } else {
            format!(
                "SELECT rowid,{select} FROM {} WHERE (?1 IS NULL OR rowid>?1) AND {predicate} ORDER BY rowid LIMIT ?3",
                spec.table
            )
        };
        let mut statement = tx
            .prepare(&sql)
            .map_err(|error| self.fault("page", sql_code(&error, budget)))?;
        let mut rows = statement
            .query(params![after, query.scope, limit + 1])
            .map_err(|error| self.fault("page", sql_code(&error, budget)))?;
        let mut records = Vec::new();
        let mut bytes = 0;
        let mut last = None;
        let mut more = false;
        while let Some(row) = rows
            .next()
            .map_err(|error| self.fault("page", sql_code(&error, budget)))?
        {
            if budget.stopped() {
                return Err(self.fault("page", budget.code()));
            }
            if records.len() == limit {
                more = true;
                break;
            }
            let mut record = Record {
                fields: Value::Object(Map::new()),
                issues: Vec::new(),
            };
            for (i, column) in columns.iter().enumerate() {
                let field = field_name(column);
                let oversized: bool = row
                    .get(2 + i * 2)
                    .map_err(|error| self.fault("page", sql_code(&error, budget)))?;
                let value = if oversized {
                    record.issues.push(format!("field_too_large:{field}"));
                    Value::Null
                } else {
                    let value = row
                        .get_ref(1 + i * 2)
                        .map_err(|error| self.fault("page", sql_code(&error, budget)))?;
                    self.cell(column, value, &mut record.issues)
                };
                record.fields[&field] = value;
            }
            let size = serde_json::to_vec(&record).expect("record JSON").len();
            if size > PAGE_BYTES {
                return Err(self.fault("page", "record_too_large"));
            }
            if bytes + size > PAGE_BYTES {
                more = true;
                break;
            }
            bytes += size;
            last = Some(
                row.get::<_, i64>(0)
                    .map_err(|error| self.fault("page", sql_code(&error, budget)))?,
            );
            records.push(record);
        }
        drop(rows);
        drop(statement);
        if revision != self.revision(&tx, budget)? {
            return Err(self.fault("page", "source_changed"));
        }
        tx.rollback()
            .map_err(|error| self.fault("page", sql_code(&error, budget)))?;
        let next_cursor = if more {
            last.map(|last| self.encode_cursor(&binding, last))
        } else {
            None
        };
        Ok(Page {
            source_id: self.source_id.clone(),
            source_revision: revision,
            records,
            next_cursor,
            issues: if self.manifest.validated_schema {
                vec![]
            } else {
                vec!["unvalidated_schema".into()]
            },
        })
    }
    fn cell(&self, column: &str, value: ValueRef<'_>, issues: &mut Vec<String>) -> Value {
        let name = field_name(column);
        let invalid = |issues: &mut Vec<String>, code| {
            issues.push(format!("{code}:{name}"));
            Value::Null
        };
        let value = match value {
            ValueRef::Null => return Value::Null,
            ValueRef::Integer(value) if super::legacy_contract::numeric(column) => {
                if matches!(column, "archived" | "runtime_status_stale") {
                    if !matches!(value, 0 | 1) {
                        return invalid(issues, "unknown_boolean");
                    }
                    Value::Bool(value == 1)
                } else {
                    Value::from(value)
                }
            }
            ValueRef::Text(text) if !super::legacy_contract::numeric(column) => {
                let Ok(text) = std::str::from_utf8(text) else {
                    return invalid(issues, "invalid_utf8");
                };
                if column.ends_with("_json") {
                    match serde_json::from_str(text) {
                        Ok(value) => value,
                        Err(_) => return invalid(issues, "invalid_json"),
                    }
                } else {
                    Value::String(text.into())
                }
            }
            _ => return invalid(issues, "invalid_type"),
        };
        if matches!(
            column,
            "capture_completeness" | "status" | "item_type" | "decode_status"
        ) {
            // Preserve source variants; interpretation belongs to the reader
            // UI. Never normalize an unknown status into completed/success.
            if value.as_str().is_none() {
                issues.push(format!("invalid_type:{name}"));
            }
        }
        super::redact::redact(&value, &self.key).0
    }
    fn require(&self, collection: Collection) -> Result<()> {
        if self
            .manifest
            .capabilities
            .iter()
            .any(|c| c.collection == collection && c.readable)
        {
            Ok(())
        } else {
            Err(self.fault("read", "unsupported_capability"))
        }
    }
    fn encode_cursor(&self, binding: &str, row: i64) -> String {
        let value = format!("{binding}:{row}");
        let mac = blake3::keyed_hash(&self.key, value.as_bytes()).to_hex();
        URL_SAFE_NO_PAD.encode(format!("{value}:{mac}"))
    }
    fn decode_cursor(&self, cursor: &str, binding: &str) -> Result<i64> {
        let error = || self.fault("page", "invalid_or_stale_cursor");
        if cursor.len() > 256 {
            return Err(error());
        }
        let decoded = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| error())?;
        let text = std::str::from_utf8(&decoded).map_err(|_| error())?;
        let (body, mac) = text.rsplit_once(':').ok_or_else(error)?;
        let (found, row) = body.split_once(':').ok_or_else(error)?;
        let expected = blake3::keyed_hash(&self.key, body.as_bytes()).to_hex();
        if found != binding
            || mac.len() != expected.len()
            || !mac
                .bytes()
                .zip(expected.bytes())
                .fold(true, |same, (a, b)| same & (a == b))
        {
            return Err(error());
        }
        row.parse().map_err(|_| error())
    }
    /// Raw attachments are addressed through an event belonging to this
    /// thread. A client-supplied blob path or unrelated blob ID is never read.
    pub fn blob(
        &self,
        thread_key: &str,
        event_seq: i64,
        expected_revision: &str,
        budget: &ReadBudget,
    ) -> Result<Record> {
        self.require(Collection::Raw)?;
        self.require(Collection::Blobs)?;
        if thread_key.len() > 4096 {
            return Err(self.fault("blob", "invalid_query"));
        }
        let tx = self.begin(budget)?;
        if self.revision(&tx, budget)? != expected_revision {
            return Err(self.fault("blob", "source_changed"));
        }
        let record = tx.query_row("SELECT b.relative_path,b.size_bytes,b.stored_hash,b.media_type FROM blobs b JOIN raw_events e ON e.blob_id=b.blob_id WHERE e.thread_key=?1 AND e.event_seq=?2", params![thread_key, event_seq], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))
            .optional().map_err(|error| self.fault("blob", sql_code(&error, budget)))?.ok_or_else(|| self.fault("blob", "not_found"))?;
        let (relative, size, hash, media) = record;
        if size < 0 || size > BLOB_BYTES as i64 {
            return Err(self.fault("blob", "blob_too_large"));
        }
        if media != "application/json" {
            return Err(self.fault("blob", "unsupported_media"));
        }
        let root = self
            .blobs
            .as_ref()
            .ok_or_else(|| self.fault("blob", "blob_root_unavailable"))?;
        let file = root
            .read_file(&relative)
            .map_err(|_| self.fault("blob", "unsafe_or_missing_blob"))?;
        let before = file
            .metadata()
            .map_err(|_| self.fault("blob", "blob_unavailable"))?;
        if before.len() != size as u64 {
            return Err(self.fault("blob", "blob_changed"));
        }
        let mut bytes = Vec::new();
        (&file)
            .take(BLOB_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| self.fault("blob", "blob_unavailable"))?;
        if budget.stopped() {
            return Err(self.fault("blob", budget.code()));
        }
        let after = file
            .metadata()
            .map_err(|_| self.fault("blob", "blob_unavailable"))?;
        if bytes.len() as i64 != size
            || files::signature(&before) != files::signature(&after)
            || blake3::hash(&bytes).to_hex().as_str() != hash
        {
            return Err(self.fault("blob", "blob_changed"));
        }
        let mut issues = Vec::new();
        let fields = self.cell("raw_json", ValueRef::Text(&bytes), &mut issues);
        if self.revision(&tx, budget)? != expected_revision {
            return Err(self.fault("blob", "source_changed"));
        }
        tx.rollback()
            .map_err(|error| self.fault("blob", sql_code(&error, budget)))?;
        Ok(Record { fields, issues })
    }
}

fn sql_code(error: &rusqlite::Error, budget: &ReadBudget) -> &'static str {
    use rusqlite::ErrorCode;
    match error.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => "source_busy",
        Some(ErrorCode::OperationInterrupted) => budget.code(),
        Some(ErrorCode::TooBig) => "record_too_large",
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => "source_corrupt",
        _ => "source_unavailable",
    }
}

fn probe(connection: &Connection) -> rusqlite::Result<Manifest> {
    let schema_version = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let mut capabilities = Vec::new();
    for contract in CONTRACTS {
        let ordinary: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_list(?1) WHERE schema='main' AND type='table' AND wr=0)", [contract.table], |r| r.get(0))?;
        let mut reasons = Vec::new();
        if !ordinary {
            reasons.push("missing_or_unsupported_table".into());
        } else {
            let mut statement =
                connection.prepare(&format!("PRAGMA table_info({})", contract.table))?;
            let columns = statement
                .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
                .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
            for column in contract.columns.split_whitespace() {
                if !columns.contains_key(column) {
                    reasons.push(format!("missing_column:{column}"));
                } else {
                    let expected = if super::legacy_contract::numeric(column) {
                        "INTEGER"
                    } else {
                        "TEXT"
                    };
                    if !columns[column].eq_ignore_ascii_case(expected) {
                        reasons.push(format!("unsupported_column_type:{column}"));
                    }
                }
            }
            if columns
                .keys()
                .any(|c| ["rowid", "oid", "_rowid_"].contains(&c.to_ascii_lowercase().as_str()))
                || connection
                    .prepare(&format!("SELECT rowid FROM {} LIMIT 0", contract.table))
                    .is_err()
            {
                reasons.push("unsupported_row_identity".into());
            }
        }
        capabilities.push(Capability {
            collection: contract.kind,
            readable: reasons.is_empty(),
            reasons,
        });
    }
    Ok(Manifest {
        contract_version: 1,
        schema_version,
        validated_schema: matches!(schema_version, 20 | 25),
        capabilities,
    })
}
