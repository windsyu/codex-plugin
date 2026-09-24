#![cfg(test)]
use super::files;
use super::legacy_reader::*;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[path = "../store/schema.rs"]
mod schema;

pub(super) struct Fixture {
    temp: TempDir,
    pub(super) path: PathBuf,
    pub(super) blobs: PathBuf,
}
impl Fixture {
    pub(super) fn new(version: i64) -> Self {
        let temp = TempDir::new().unwrap();
        // URI punctuation/Unicode must stay filename data, never query options.
        let path = temp.path().join("历史 ?mode=rw#库.sqlite");
        let blobs = temp.path().join("blobs");
        fs::create_dir(&blobs).unwrap();
        let connection = Connection::open(&path).unwrap();
        for (_, sql) in schema::migrations() {
            connection.execute_batch(sql).unwrap();
        }
        connection.execute_batch("INSERT INTO sources VALUES ('source','rollout','synthetic','{\"token\":\"fixture-secret\"}','ready',1,NULL,1,1);
          INSERT INTO source_epochs(source_id,epoch_id,opened_at_ms,capability_json) VALUES('source','epoch',1,'{}');
          INSERT INTO threads(thread_key,store_source_id,codex_thread_id,name,cwd,project_key,last_event_seq) VALUES
            ('thread-a','source','native-a','Synthetic A','/synthetic/a','project-a',1),
            ('thread-b','source','native-b','Synthetic B','/synthetic/b','project-b',2),
            ('thread-c','source','native-c','Synthetic C',NULL,NULL,3);
          UPDATE threads SET parent_thread_key='thread-a',base_instructions_json='{\"text\":\"synthetic instructions\",\"token\":\"fixture-secret\"}' WHERE thread_key='thread-b';
          INSERT INTO turns(thread_key,turn_id,status,capture_completeness,completeness_reasons_json,coverage_json,projection_json,provenance_json,last_event_seq) VALUES ('thread-a','turn-a','unknown','partial','[\"synthetic_gap\"]','{}','{}','{}',1);
          INSERT INTO items(thread_key,turn_scope,item_id,turn_id,item_type,status,summary_text,projection_json,provenance_json,last_event_seq) VALUES
            ('thread-a','turn-a','item-a','turn-a','userMessage','completed','Hello','{\"text\":\"Hello\"}','{}',1),
            ('thread-a','turn-a','item-b','turn-a','futureVariant','futureStatus','World','{\"futureField\":true}','{}',2);
          INSERT INTO raw_events(event_id,source_id,epoch_id,source_seq,dedupe_key,observed_at_ms,thread_key,codex_thread_id,method,phase,source_fingerprint,stored_raw_hash,raw_json,redaction_json,decode_status) VALUES('event-a','source','epoch',1,'dedupe',1,'thread-a','native-a','rollout/future','observed','synthetic','hash','{\"content\":\"Hello\",\"token\":\"fixture-secret\"}','{}','unknown');
          INSERT INTO ingest_errors(error_id,source_id,epoch_id,offset_or_seq,category,message,preview,first_seen_at_ms,last_seen_at_ms,occurrence_count) VALUES('gap','source','epoch',8,'unknown','fixture-secret','fixture-secret',1,1,1);
          INSERT INTO maintenance_audit VALUES('audit','sentinel','source','source',1,'{\"untouched\":true}');
          INSERT INTO gateway_commands(command_id,principal_id,capability,idempotency_key,payload_hash,source_id,source_epoch,input_summary_json,state,created_at_ms,updated_at_ms) VALUES('command','synthetic','read','key','hash','source','epoch','{}','received',1,1);
          INSERT INTO control_audit(command_id,principal_id,capability,source_id,source_epoch,decision,outcome,payload_hash,input_summary_json,occurred_at_ms) VALUES('command','synthetic','read','source','epoch','sentinel','sentinel','hash','{}',1);") .unwrap();
        if version == 25 {
            // Shape-only extras observed in schema 25. No control behavior or
            // guessed historical migrations are recreated.
            for table in [
                "native_upload_events",
                "native_upload_uses",
                "native_uploads",
                "session_goal_owner_transitions",
                "session_goal_owners",
                "session_input_queue",
                "session_input_queue_transitions",
            ] {
                connection.execute_batch(&format!("CREATE TABLE {table}(sentinel TEXT); INSERT INTO {table} VALUES('untouched');")).unwrap();
            }
            connection
                .execute_batch("ALTER TABLE image_uploads ADD COLUMN queue_entry_id TEXT;")
                .unwrap();
        }
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
            .unwrap();
        drop(connection);
        Self { temp, path, blobs }
    }
    fn reader(&self) -> LegacyReader {
        LegacyReader::open(
            "legacy",
            &self.path,
            Some(&self.blobs),
            &ReadBudget::default(),
        )
        .unwrap()
    }
    fn edit(&self, sql: &str) {
        Connection::open(&self.path)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
    fn blob(&self, relative: &str, value: &[u8]) {
        let hash = blake3::hash(value).to_hex().to_string();
        let connection = Connection::open(&self.path).unwrap();
        connection.execute("INSERT OR REPLACE INTO blobs(blob_id,stored_hash,media_type,size_bytes,relative_path,created_event_seq,created_at_ms) VALUES('blob',?1,'application/json',?2,?3,1,1)", rusqlite::params![hash, value.len(), relative]).unwrap();
        connection
            .execute("UPDATE raw_events SET blob_id='blob' WHERE event_seq=1", [])
            .unwrap();
    }
}
fn query(collection: Collection, scope: Option<&str>) -> Query {
    Query {
        collection,
        scope: scope.map(str::to_owned),
    }
}
fn page(reader: &LegacyReader, collection: Collection, scope: Option<&str>) -> Page {
    reader
        .page(&query(collection, scope), None, 100, &ReadBudget::default())
        .unwrap()
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, [u64; 7])> {
    let mut output = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            output.insert(
                entry.path().strip_prefix(root).unwrap().into(),
                (
                    fs::read(entry.path()).unwrap(),
                    files::signature(&entry.metadata().unwrap()),
                ),
            );
        }
    }
    output
}

#[test]
fn schema20_and_25_contracts_match_and_preserve_every_file() {
    let mut baseline = None;
    for version in [20, 25] {
        let fixture = Fixture::new(version);
        let before = snapshot(fixture.temp.path());
        let reader = fixture.reader();
        assert_eq!(reader.manifest().schema_version, version);
        assert!(reader.manifest().validated_schema);
        assert!(
            reader
                .manifest()
                .capabilities
                .iter()
                .all(|capability| capability.readable)
        );
        let mut output = Vec::new();
        for (kind, scope) in [
            (Collection::Projects, None),
            (Collection::Threads, None),
            (Collection::Context, Some("thread-b")),
            (Collection::Relations, Some("thread-b")),
            (Collection::Turns, Some("thread-a")),
            (Collection::Items, Some("thread-a")),
            (Collection::Sources, None),
            (Collection::Epochs, Some("source")),
            (Collection::Gaps, Some("source")),
            (Collection::Raw, Some("thread-a")),
        ] {
            let result = page(&reader, kind, scope);
            assert!(!result.records.is_empty(), "{kind:?}");
            output.push(serde_json::to_value(result.records).unwrap());
        }
        assert!(
            !serde_json::to_string(&output)
                .unwrap()
                .contains("fixture-secret")
        );
        assert_eq!(output[5][1]["fields"]["itemType"], "futureVariant");
        assert_eq!(output[5][1]["fields"]["status"], "futureStatus");
        if let Some(ref baseline) = baseline {
            assert_eq!(&output, baseline);
        } else {
            baseline = Some(output);
        }
        drop(reader);
        assert_eq!(snapshot(fixture.temp.path()), before);
    }
}

#[test]
fn keyset_pages_bind_source_filter_collection_and_revision() {
    let fixture = Fixture::new(20);
    let reader = fixture.reader();
    for collection in [Collection::Projects, Collection::Threads, Collection::Items] {
        let q = query(
            collection,
            (collection == Collection::Items).then_some("thread-a"),
        );
        let first = reader.page(&q, None, 1, &ReadBudget::default()).unwrap();
        let cursor = first.next_cursor.unwrap();
        let second = reader
            .page(&q, Some(&cursor), 1, &ReadBudget::default())
            .unwrap();
        assert_ne!(first.records[0].fields, second.records[0].fields);
        assert_eq!(
            reader
                .page(
                    &query(collection, Some("different")),
                    Some(&cursor),
                    1,
                    &ReadBudget::default()
                )
                .unwrap_err()
                .code,
            "invalid_or_stale_cursor"
        );
        assert!(
            fixture
                .reader()
                .page(&q, Some(&cursor), 1, &ReadBudget::default())
                .is_err()
        );
        assert!(
            reader
                .page(&q, Some(&(cursor.clone() + "x")), 1, &ReadBudget::default())
                .is_err()
        );
    }
    let first = reader
        .page(
            &query(Collection::Threads, None),
            None,
            1,
            &ReadBudget::default(),
        )
        .unwrap();
    fixture.edit("UPDATE threads SET name='changed' WHERE thread_key='thread-a'");
    assert_eq!(
        reader
            .page(
                &query(Collection::Threads, None),
                first.next_cursor.as_deref(),
                1,
                &ReadBudget::default()
            )
            .unwrap_err()
            .code,
        "invalid_or_stale_cursor"
    );
    assert_eq!(
        page(&reader, Collection::Threads, None).records[0].fields["name"],
        "changed"
    );
}

#[test]
fn missing_columns_unknown_versions_and_bad_json_are_local_degradation() {
    let fixture = Fixture::new(99);
    fixture.edit("ALTER TABLE threads DROP COLUMN base_instructions_json; UPDATE items SET projection_json='{bad-json-private';");
    let reader = fixture.reader();
    assert!(!reader.manifest().validated_schema);
    assert!(
        !reader
            .manifest()
            .capabilities
            .iter()
            .find(|c| c.collection == Collection::Context)
            .unwrap()
            .readable
    );
    assert_eq!(
        reader
            .page(
                &query(Collection::Context, Some("thread-a")),
                None,
                1,
                &ReadBudget::default()
            )
            .unwrap_err()
            .code,
        "unsupported_capability"
    );
    let result = page(&reader, Collection::Items, Some("thread-a"));
    assert_eq!(result.issues, vec!["unvalidated_schema"]);
    assert!(result.records[0].fields["raw"].is_null());
    assert_eq!(result.records[0].issues, vec!["invalid_json:raw"]);
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("bad-json-private")
    );
    assert_eq!(page(&reader, Collection::Threads, None).records.len(), 3);
}

#[test]
fn views_and_rowid_spoofing_do_not_become_history_tables() {
    let fixture = Fixture::new(20);
    fixture.edit("ALTER TABLE items RENAME TO hidden_items; CREATE VIEW items AS SELECT * FROM hidden_items; ALTER TABLE sources ADD COLUMN rowid TEXT;");
    let reader = fixture.reader();
    for kind in [Collection::Items, Collection::Sources] {
        assert_eq!(
            reader
                .page(
                    &query(kind, Some("thread-a")),
                    None,
                    1,
                    &ReadBudget::default()
                )
                .unwrap_err()
                .code,
            "unsupported_capability"
        );
    }
}

#[test]
fn cancellation_timeout_oversize_and_invalid_input_are_bounded() {
    let fixture = Fixture::new(20);
    let reader = fixture.reader();
    let cancelled = ReadBudget::default();
    cancelled.cancel();
    assert_eq!(
        reader
            .page(&query(Collection::Threads, None), None, 1, &cancelled)
            .unwrap_err()
            .code,
        "cancelled"
    );
    assert_eq!(
        reader
            .page(
                &query(Collection::Threads, None),
                None,
                1,
                &ReadBudget::new(Duration::ZERO)
            )
            .unwrap_err()
            .code,
        "query_timeout"
    );
    assert!(
        reader
            .page(
                &query(Collection::Items, None),
                None,
                1,
                &ReadBudget::default()
            )
            .is_err()
    );
    assert!(
        reader
            .page(
                &query(Collection::Threads, None),
                None,
                101,
                &ReadBudget::default()
            )
            .is_err()
    );
    fixture.edit("UPDATE items SET summary_text=hex(zeroblob(150000));");
    let result = page(&reader, Collection::Items, Some("thread-a"));
    assert!(result.records[0].fields["summaryText"].is_null());
    assert!(
        result.records[0]
            .issues
            .contains(&"field_too_large:summaryText".into())
    );
    let large = Connection::open(&fixture.path).unwrap();
    large.execute_batch("WITH RECURSIVE nums(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM nums WHERE x<50000) INSERT INTO threads(thread_key,store_source_id,codex_thread_id,last_event_seq,project_key) SELECT 'bulk-'||x,'bulk','bulk-'||x,0,'project-'||x FROM nums;").unwrap();
    drop(large);
    assert_eq!(
        reader
            .page(
                &query(Collection::Projects, None),
                None,
                1,
                &ReadBudget::new(Duration::from_millis(1))
            )
            .unwrap_err()
            .code,
        "query_timeout"
    );
    let budget = ReadBudget::default();
    let cancel = budget.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2));
        cancel.cancel();
    });
    let result = reader.page(&query(Collection::Projects, None), None, 1, &budget);
    canceller.join().unwrap();
    assert_eq!(result.unwrap_err().code, "cancelled");
    // A stopped query must release its read transaction and permit reuse.
    assert_eq!(
        reader
            .page(
                &query(Collection::Threads, None),
                None,
                1,
                &ReadBudget::default()
            )
            .unwrap()
            .records
            .len(),
        1
    );
}

#[test]
fn source_replacement_and_schema_changes_invalidate_the_reader() {
    let fixture = Fixture::new(20);
    let reader = fixture.reader();
    fixture.edit("ALTER TABLE threads ADD COLUMN future TEXT");
    assert_eq!(
        reader
            .page(
                &query(Collection::Threads, None),
                None,
                1,
                &ReadBudget::default()
            )
            .unwrap_err()
            .code,
        "schema_changed"
    );
    let reader = fixture.reader();
    fs::rename(&fixture.path, fixture.path.with_extension("old")).unwrap();
    fs::copy(fixture.path.with_extension("old"), &fixture.path).unwrap();
    assert_eq!(
        reader
            .page(
                &query(Collection::Threads, None),
                None,
                1,
                &ReadBudget::default()
            )
            .unwrap_err()
            .code,
        "source_replaced"
    );
}

#[test]
fn attachments_require_event_scope_hash_and_safe_components() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new(25);
    let content = br#"{"text":"synthetic attachment","token":"fixture-secret"}"#;
    fs::create_dir(fixture.blobs.join("aa")).unwrap();
    fs::write(fixture.blobs.join("aa/good.json"), content).unwrap();
    fixture.blob("aa/good.json", content);
    let before = snapshot(fixture.temp.path());
    let reader = fixture.reader();
    let revision = page(&reader, Collection::Raw, Some("thread-a")).source_revision;
    let result = reader
        .blob("thread-a", 1, &revision, &ReadBudget::default())
        .unwrap();
    assert_eq!(result.fields["text"], "synthetic attachment");
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("fixture-secret")
    );
    assert_eq!(
        reader
            .blob("thread-b", 1, &revision, &ReadBudget::default())
            .unwrap_err()
            .code,
        "not_found"
    );
    drop(reader);
    assert_eq!(before, snapshot(fixture.temp.path()));
    fs::write(fixture.temp.path().join("outside.json"), content).unwrap();
    symlink(fixture.temp.path(), fixture.blobs.join("escape")).unwrap();
    symlink(
        fixture.temp.path().join("outside.json"),
        fixture.blobs.join("symlink.json"),
    )
    .unwrap();
    fs::hard_link(
        fixture.temp.path().join("outside.json"),
        fixture.blobs.join("hardlink.json"),
    )
    .unwrap();
    for path in [
        "../outside.json",
        "/outside.json",
        "escape/outside.json",
        "symlink.json",
        "hardlink.json",
        "aa//good.json",
        "aa/./good.json",
    ] {
        fixture.blob(path, content);
        let reader = fixture.reader();
        let revision = page(&reader, Collection::Raw, Some("thread-a")).source_revision;
        assert_eq!(
            reader
                .blob("thread-a", 1, &revision, &ReadBudget::default())
                .unwrap_err()
                .code,
            "unsafe_or_missing_blob",
            "{path}"
        );
    }
    fixture.blob("aa/good.json", b"wrong length");
    let reader = fixture.reader();
    let revision = page(&reader, Collection::Raw, Some("thread-a")).source_revision;
    assert_eq!(
        reader
            .blob("thread-a", 1, &revision, &ReadBudget::default())
            .unwrap_err()
            .code,
        "blob_changed"
    );
}

// A distinct process is essential: SQLite's native VFS shares SHM nodes for
// same-process connections. Production never starts the old writer internally.
struct Writer {
    child: Child,
    input: ChildStdin,
    output: Receiver<String>,
}
impl Writer {
    fn start(path: &Path, mode: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "history::tests::writer_child",
                "--ignored",
                "--nocapture",
            ])
            .env("R6_WRITER_PATH", path)
            .env("R6_WRITER_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, output) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                let _ = send.send(line);
            }
        });
        let writer = Self {
            child,
            input,
            output,
        };
        writer.wait("READY");
        writer
    }
    fn wait(&self, expected: &str) {
        loop {
            if self.output.recv_timeout(Duration::from_secs(5)).unwrap() == expected {
                break;
            }
        }
    }
    fn command(&mut self, command: &str) {
        writeln!(self.input, "{command}").unwrap();
        self.input.flush().unwrap();
        self.wait("DONE");
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "subprocess harness; invoked by WAL/locking probes with temporary paths"]
fn writer_child() {
    let path = std::env::var_os("R6_WRITER_PATH").expect("probe path");
    let connection = Connection::open(path).unwrap();
    let mode = std::env::var("R6_WRITER_MODE").unwrap();
    if mode == "wal" {
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; UPDATE threads SET name='wal-visible' WHERE thread_key='thread-a';").unwrap();
    }
    println!("READY");
    std::io::stdout().flush().unwrap();
    for line in std::io::stdin().lock().lines() {
        match line.unwrap().as_str() {
            "write" => {
                connection
                    .execute_batch("UPDATE threads SET name='wal-new' WHERE thread_key='thread-a';")
                    .unwrap();
            }
            "begin" => {
                connection.execute_batch("BEGIN IMMEDIATE; UPDATE threads SET name='uncommitted' WHERE thread_key='thread-a';").unwrap();
            }
            "exclusive" => {
                connection.execute_batch("BEGIN EXCLUSIVE;").unwrap();
            }
            "dirty" => {
                connection.execute_batch("PRAGMA cache_size=1; BEGIN IMMEDIATE; UPDATE threads SET last_message_preview=hex(randomblob(131072));").unwrap();
            }
            "rollback" => {
                connection.execute_batch("ROLLBACK;").unwrap();
            }
            "checkpoint" => {
                connection
                    .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                    .unwrap();
            }
            _ => panic!("invalid probe command"),
        }
        println!("DONE");
        std::io::stdout().flush().unwrap();
    }
}

#[test]
fn wal_concurrent_writer_snapshots_and_reader_close_are_zero_write() {
    let fixture = Fixture::new(25);
    let mut writer = Writer::start(&fixture.path, "wal");
    let before = snapshot(fixture.temp.path());
    let reader = fixture.reader();
    assert_eq!(
        page(&reader, Collection::Threads, None).records[0].fields["name"],
        "wal-visible"
    );
    assert_eq!(before, snapshot(fixture.temp.path()));
    writer.command("begin");
    let before = snapshot(fixture.temp.path());
    assert_eq!(
        page(&reader, Collection::Threads, None).records[0].fields["name"],
        "wal-visible"
    );
    assert_eq!(before, snapshot(fixture.temp.path()));
    writer.command("rollback");
    let first = reader
        .page(
            &query(Collection::Threads, None),
            None,
            1,
            &ReadBudget::default(),
        )
        .unwrap();
    writer.command("write");
    let before = snapshot(fixture.temp.path());
    assert_eq!(
        reader
            .page(
                &query(Collection::Threads, None),
                first.next_cursor.as_deref(),
                1,
                &ReadBudget::default()
            )
            .unwrap_err()
            .code,
        "invalid_or_stale_cursor"
    );
    assert_eq!(
        page(&reader, Collection::Threads, None).records[0].fields["name"],
        "wal-new"
    );
    drop(reader);
    assert_eq!(before, snapshot(fixture.temp.path()));
    drop(writer); // leave WAL/SHM as if the external writer crashed
    let before = snapshot(fixture.temp.path());
    let reader = fixture.reader();
    assert_eq!(
        page(&reader, Collection::Threads, None).records[0].fields["name"],
        "wal-new"
    );
    drop(reader);
    assert_eq!(before, snapshot(fixture.temp.path()));
}

#[test]
fn missing_shm_or_wal_never_creates_auxiliary_files() {
    for suffix in ["-shm", "-wal"] {
        let fixture = Fixture::new(20);
        let writer = Writer::start(&fixture.path, "wal");
        drop(writer);
        let mut auxiliary = fixture.path.as_os_str().to_owned();
        auxiliary.push(suffix);
        fs::remove_file(&auxiliary).unwrap();
        let before = snapshot(fixture.temp.path());
        let result = LegacyReader::open("legacy", &fixture.path, None, &ReadBudget::default());
        assert!(
            result.is_err(),
            "missing {suffix} must not be repaired by reader"
        );
        drop(result);
        assert_eq!(before, snapshot(fixture.temp.path()));
    }
}

#[test]
fn same_process_writable_shm_fails_closed_and_locks_are_bounded() {
    let fixture = Fixture::new(20);
    let writer = Connection::open(&fixture.path).unwrap();
    writer.execute_batch("PRAGMA journal_mode=WAL; UPDATE threads SET name='in-process' WHERE thread_key='thread-a';").unwrap();
    let before = snapshot(fixture.temp.path());
    assert!(LegacyReader::open("legacy", &fixture.path, None, &ReadBudget::default()).is_err());
    assert_eq!(before, snapshot(fixture.temp.path()));
    drop(writer);
    fixture.edit("PRAGMA journal_mode=DELETE");
    let reader = fixture.reader();
    let mut writer = Writer::start(&fixture.path, "delete");
    writer.command("exclusive");
    let before = snapshot(fixture.temp.path());
    let start = Instant::now();
    assert_eq!(
        reader
            .page(
                &query(Collection::Threads, None),
                None,
                1,
                &ReadBudget::default()
            )
            .unwrap_err()
            .code,
        "source_busy"
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(before, snapshot(fixture.temp.path()));
    writer.command("rollback");
    assert_eq!(page(&reader, Collection::Threads, None).records.len(), 3);
}

#[test]
fn read_vfs_denies_mutation_even_without_query_only() {
    let fixture = Fixture::new(20);
    super::readonly_vfs::register().unwrap();
    let mut uri = reqwest::Url::from_file_path(&fixture.path).unwrap();
    uri.set_query(Some("mode=ro&readonly_shm=1"));
    let before = snapshot(fixture.temp.path());
    let connection = Connection::open_with_flags_and_vfs(
        uri.as_str(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        super::readonly_vfs::NAME,
    )
    .unwrap();
    connection.execute_batch("PRAGMA query_only=OFF").unwrap();
    for sql in [
        "DELETE FROM threads",
        "PRAGMA user_version=20",
        "PRAGMA journal_mode=WAL",
        "DROP TABLE maintenance_audit",
        "VACUUM",
        "SELECT load_extension('synthetic')",
    ] {
        assert!(connection.execute_batch(sql).is_err(), "{sql}");
    }
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM threads", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    drop(connection);
    assert_eq!(before, snapshot(fixture.temp.path()));
}

#[test]
fn hot_journal_is_not_recovered_or_deleted_by_the_reader() {
    let fixture = Fixture::new(20);
    let mut writer = Writer::start(&fixture.path, "delete");
    writer.command("dirty");
    drop(writer);
    let mut journal = fixture.path.as_os_str().to_owned();
    journal.push("-journal");
    assert!(fs::metadata(&journal).unwrap().len() > 512);
    let before = snapshot(fixture.temp.path());
    assert!(LegacyReader::open("legacy", &fixture.path, None, &ReadBudget::default()).is_err());
    assert_eq!(before, snapshot(fixture.temp.path()));
}

#[test]
fn unsafe_sources_and_missing_attachments_do_not_expand_access() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new(20);
    let missing = fixture.temp.path().join("does-not-exist");
    assert!(LegacyReader::open("legacy", &missing, None, &ReadBudget::default()).is_err());
    assert!(!missing.exists());
    let alias = fixture.temp.path().join("alias");
    symlink(&fixture.path, &alias).unwrap();
    assert!(LegacyReader::open("legacy", &alias, None, &ReadBudget::default()).is_err());
    let reader = LegacyReader::open(
        "legacy",
        &fixture.path,
        Some(&missing),
        &ReadBudget::default(),
    )
    .unwrap();
    assert!(
        !reader
            .manifest()
            .capabilities
            .iter()
            .find(|c| c.collection == Collection::Blobs)
            .unwrap()
            .readable
    );
    assert_eq!(page(&reader, Collection::Threads, None).records.len(), 3);
    let before = snapshot(fixture.temp.path());
    assert!(
        page(&reader, Collection::Threads, Some("project-a' OR 1=1 --"))
            .records
            .is_empty()
    );
    drop(reader);
    assert_eq!(before, snapshot(fixture.temp.path()));
}

#[test]
fn malformed_scalar_values_and_unknown_columns_are_not_success_defaults() {
    let fixture = Fixture::new(25);
    fixture.edit("ALTER TABLE threads ADD COLUMN future_flag TEXT; UPDATE threads SET archived=7,last_event_seq='not-an-integer';");
    let reader = fixture.reader();
    let result = page(&reader, Collection::Threads, None);
    assert!(result.records[0].fields["archived"].is_null());
    assert!(result.records[0].fields["lastEventSeq"].is_null());
    assert!(
        result.records[0]
            .issues
            .contains(&"unknown_boolean:archived".into())
    );
    assert!(
        result.records[0]
            .issues
            .contains(&"invalid_type:lastEventSeq".into())
    );
    assert!(result.records[0].fields.get("futureFlag").is_none());
}
