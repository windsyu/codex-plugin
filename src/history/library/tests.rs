#![cfg(test)]
use super::*;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use tempfile::TempDir;

fn rollout(path: &Path, id: uuid::Uuid, cwd: &Path, text: &str, tail: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path,format!("{}\n{}\n{}",json!({"type":"session_meta","payload":{"id":id,"session_id":uuid::Uuid::new_v4(),"cwd":cwd,"timestamp":"2026-09-22T01:00:00Z"}}),json!({"type":"response_item","timestamp":"2026-09-22T02:00:00Z","payload":{"type":"message","role":"user","content":[{"text":text}]}}),tail)).unwrap();
}
fn fixture() -> (
    TempDir,
    PathBuf,
    PathBuf,
    Arc<RwLock<LibraryConfig>>,
    HistoryLibrary,
) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("native");
    fs::create_dir(&home).unwrap();
    let data = temp.path().join("history");
    let config = Arc::new(RwLock::new(LibraryConfig::default()));
    let policy = config.clone();
    let library = HistoryLibrary::with_policy(
        home.clone(),
        data.clone(),
        Arc::new(move || Ok(policy.read().unwrap().clone())),
    )
    .unwrap();
    (temp, home, data, config, library)
}
fn wait(library: &HistoryLibrary, count: usize) -> Value {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let result = query(
            &library.handle.shared,
            Operation::List(Query::default(), false),
        );
        if let Ok(value) = result
            && value["records"].as_array().unwrap().len() == count
            && library
                .handle
                .statuses()
                .iter()
                .all(|s| s.state != "indexing")
        {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "statuses {:?}",
            library.handle.statuses()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn refresh(library: &HistoryLibrary) {
    let _ = query(&library.handle.shared, Operation::Refresh);
    std::thread::sleep(Duration::from_millis(150));
}
#[test]
fn incomplete_discovery_does_not_clear_project_selection_and_counts_follow_filters() {
    let (_temp, home, data, _config, library) = fixture();
    for (name, parent) in [("main", false), ("agent", true)] {
        let file = home.join(format!("sessions/{name}.jsonl"));
        rollout(
            &file,
            uuid::Uuid::new_v4(),
            Path::new(""),
            &format!("needle {name}"),
            "",
        );
        if parent {
            let raw = fs::read_to_string(&file).unwrap().replace(
                "\"cwd\":",
                &format!("\"parent_thread_id\":\"{}\",\"cwd\":", uuid::Uuid::new_v4()),
            );
            fs::write(&file, raw).unwrap();
        }
    }
    refresh(&library);
    wait(&library, 2);
    for (source, group, q, count) in [
        (None, None, None, 2),
        (Some("default-native"), Some("agents"), None, 1),
        (None, Some("main"), Some("needle main"), 1),
        (Some("default-workbench"), None, None, 0),
        (None, None, Some("absent"), 0),
    ] {
        let result = query(
            &library.handle.shared,
            Operation::List(
                Query {
                    source_id: source.map(String::from),
                    group: group.map(String::from),
                    q: q.map(String::from),
                    ..Default::default()
                },
                true,
            ),
        )
        .unwrap();
        assert_eq!(result["unassignedRecords"], count);
    }
    // A published but truncated catalog cannot prove an old project no longer exists.
    let shared = library.handle.shared.clone();
    drop(library);
    let db = Connection::open(data.join("library/catalog.sqlite")).unwrap();
    db.execute(
        "UPDATE catalog_sources SET issues='[\"cache_budget\"]' WHERE id='default-native'",
        [],
    )
    .unwrap();
    drop(db);
    let result = query(
        &shared,
        Operation::List(
            Query {
                project_id: Some("p_missing".into()),
                ..Default::default()
            },
            false,
        ),
    )
    .unwrap();
    assert!(result.get("projectExists").is_none());
}

#[test]
fn reindexing_keeps_published_revision_only_for_the_same_source_identity() {
    let (_temp, home, _data, _config, library) = fixture();
    rollout(
        &home.join("sessions/a.jsonl"),
        uuid::Uuid::new_v4(),
        Path::new("/synthetic"),
        "ready record",
        "",
    );
    refresh(&library);
    wait(&library, 1);
    let source = library.handle.shared.policy().unwrap().1[0].clone();
    let original = library
        .handle
        .statuses()
        .into_iter()
        .find(|s| s.id == source.id())
        .unwrap()
        .revision
        .unwrap();
    library
        .handle
        .shared
        .set(status(&source, "indexing", "refresh", 1, None));
    let progress = library
        .handle
        .statuses()
        .into_iter()
        .find(|s| s.id == source.id())
        .unwrap();
    assert_eq!(progress.revision.as_deref(), Some(original.as_str()));
    assert_eq!(progress.indexed_entries, 1);
    let changed = Source::Native {
        id: source.id().into(),
        codex_home: home.join("replacement"),
    };
    library
        .handle
        .shared
        .set(status(&changed, "indexing", "changed source", 0, None));
    assert!(
        library
            .handle
            .shared
            .status
            .read()
            .unwrap()
            .iter()
            .find(|s| s.id == source.id())
            .unwrap()
            .revision
            .is_none()
    );
}

#[test]
fn read_only_instance_reports_published_revision_and_count() {
    let (_temp, home, data, config, library) = fixture();
    rollout(
        &home.join("sessions/a.jsonl"),
        uuid::Uuid::new_v4(),
        Path::new("/synthetic"),
        "shared cache",
        "",
    );
    refresh(&library);
    wait(&library, 1);
    let original = library
        .handle
        .statuses()
        .into_iter()
        .find(|s| s.id == "default-native")
        .unwrap()
        .revision
        .unwrap();
    let second = HistoryLibrary::with_policy(
        home,
        data,
        Arc::new(move || Ok(config.read().unwrap().clone())),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(source) =
            second.handle.statuses().into_iter().find(|s| {
                s.id == "default-native" && s.state == "read_only" && s.revision.is_some()
            })
        {
            assert_eq!(source.revision.as_deref(), Some(original.as_str()));
            assert_eq!(source.indexed_entries, 1);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn cwd_index_bytes_are_admitted_and_reused_within_the_same_budget() {
    let (_temp, _home, _data, _config, library) = fixture();
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE catalog_records(generation TEXT,entry TEXT,seq INTEGER,body TEXT); CREATE TABLE catalog_locators(generation TEXT,entry TEXT,locator TEXT); CREATE TABLE catalog_cwds(generation TEXT,entry TEXT,cwd_key TEXT,path TEXT);").unwrap();
    let mut db = db;
    let source = Source::Native {
        id: "n".into(),
        codex_home: "/synthetic".into(),
    };
    let mut entry = Entry::new(&source, "a");
    entry.native_path(
        Some("/synthetic/Documents/Codex/2026-09-23/chat"),
        Some("Codex Desktop"),
    );
    let budget = serde_json::to_vec(&entry).unwrap().len();
    let doc = Document {
        entry,
        records: vec![],
        locator: json!({}),
    };
    assert_eq!(
        write_document(&mut db, "g", &doc, None, budget).unwrap_err(),
        "cache_budget"
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM catalog_cwds", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0,
        "failed admission rolls back the whole document"
    );
    drop(library);
}

#[test]
#[ignore = "read-only metadata audit; requires explicit WORKBENCH_NATIVE_METADATA_HOME"]
fn native_metadata_classification_read_only_probe() {
    use std::io::{BufRead, BufReader};
    let home = PathBuf::from(
        std::env::var_os("WORKBENCH_NATIVE_METADATA_HOME").expect("explicit readonly home"),
    );
    let source = Source::Native {
        id: "audit".into(),
        codex_home: home.clone(),
    };
    let mut matched = 0;
    let mut total = 0;
    let mut directories = std::collections::BTreeSet::new();
    let mut unreadable = 0;
    for root in [home.join("sessions"), home.join("archived_sessions")] {
        if !root.exists() {
            continue;
        }
        for item in walkdir::WalkDir::new(root)
            .follow_links(false)
            .max_depth(12)
        {
            let item = item.unwrap();
            if !item.file_type().is_file() || item.path().extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let signature = crate::history::files::signature(&item.metadata().unwrap());
            let file = fs::File::open(item.path()).unwrap();
            let mut reader = BufReader::new(std::io::Read::take(file, 1024 * 1024));
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let Ok(meta) = serde_json::from_str::<Value>(&line) else {
                unreadable += 1;
                continue;
            };
            if meta["type"] != "session_meta" {
                unreadable += 1;
                continue;
            }
            total += 1;
            let payload = &meta["payload"];
            let mut entry = Entry::new(&source, "metadata-only");
            entry.native_path(payload["cwd"].as_str(), payload["originator"].as_str());
            if entry.project_basis == "desktop_generated" {
                matched += 1;
                assert!(entry.project_id.is_none());
                directories.insert(entry.recorded_cwd);
            }
            assert_eq!(
                crate::history::files::signature(&fs::metadata(item.path()).unwrap()),
                signature
            );
        }
    }
    println!(
        "{}",
        json!({"metadataFiles":total,"desktopUnassigned":matched,"desktopDirectories":directories.len(),"unreadableHeaders":unreadable,"sourceWrites":0})
    );
}
#[test]
fn classification_cache_upgrade_rebuilds_unchanged_rollout_and_preserves_identity() {
    let (_temp, home, data, config, library) = fixture();
    let file = home.join("sessions/desktop.jsonl");
    let id = uuid::Uuid::new_v4();
    let cwd = "/synthetic/Documents/Codex/2026-09-23/new-chat";
    rollout(&file, id, Path::new(cwd), "unchanged body", "");
    let text = fs::read_to_string(&file)
        .unwrap()
        .replace("\"cwd\":", "\"originator\":\"Codex Desktop\",\"cwd\":");
    fs::write(&file, text).unwrap();
    let before = (
        fs::read(&file).unwrap(),
        crate::history::files::signature(&fs::metadata(&file).unwrap()),
    );
    refresh(&library);
    let original = wait(&library, 1);
    let entry = original["records"][0]["entryId"]
        .as_str()
        .unwrap()
        .to_string();
    drop(library);
    // Recreate the version-1 projection and its matching checkpoint; raw rollout is untouched.
    let db = Connection::open(data.join("library/catalog.sqlite")).unwrap();
    let (project_id, _) = model::project(cwd).unwrap();
    db.execute("UPDATE catalog_entries SET project=?1, metadata=json_remove(json_set(metadata,'$.projectId',?1,'$.projectPath',?2),'$.recordedCwd','$.projectBasis')",params![project_id,cwd]).unwrap();
    db.execute_batch("DROP TABLE catalog_cwds; PRAGMA user_version=1;")
        .unwrap();
    drop(db);
    let policy = config.clone();
    let shared = Shared {
        home: home.clone(),
        data: data.clone(),
        policy: Arc::new(move || Ok(policy.read().unwrap().clone())),
        status: RwLock::new(vec![]),
        stop: AtomicBool::new(false),
        refresh: AtomicBool::new(false),
        key: [7; 32],
    };
    assert!(
        read_db(&shared).unwrap().is_none(),
        "old classification is not served as current"
    );
    let policy = config.clone();
    let library = HistoryLibrary::with_policy(
        home,
        data.clone(),
        Arc::new(move || Ok(policy.read().unwrap().clone())),
    )
    .unwrap();
    let rebuilt = wait(&library, 1);
    assert_eq!(rebuilt["records"][0]["entryId"], entry);
    assert!(rebuilt["records"][0]["projectId"].is_null());
    assert_eq!(rebuilt["records"][0]["recordedCwd"], cwd);
    assert!(
        query(
            &library.handle.shared,
            Operation::Body(entry, Query::default())
        )
        .unwrap()
        .to_string()
        .contains("unchanged body")
    );
    refresh(&library);
    let refreshed = wait(&library, 1);
    assert_eq!(refreshed["records"], rebuilt["records"]);
    assert_eq!(refreshed["revision"], rebuilt["revision"]);
    assert_eq!(
        (
            fs::read(&file).unwrap(),
            crate::history::files::signature(&fs::metadata(&file).unwrap())
        ),
        before
    );
    let db = Connection::open(data.join("library/catalog.sqlite")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        CATALOG_VERSION
    );
}

#[test]
fn legacy_recorded_null_project_remains_unassigned_with_cwd_without_source_writes() {
    for version in [20, 25] {
        let legacy = crate::history::tests::Fixture::new(version);
        let db = Connection::open(&legacy.path).unwrap();
        db.execute_batch("UPDATE threads SET project_key=NULL,cwd='/synthetic/execution' WHERE thread_key='thread-a'; UPDATE threads SET project_key='/synthetic/project',cwd='/synthetic/different' WHERE thread_key='thread-b';").unwrap();
        drop(db);
        let before = (
            fs::read(&legacy.path).unwrap(),
            crate::history::files::signature(&fs::metadata(&legacy.path).unwrap()),
        );
        let (_temp, _home, _data, config, library) = fixture();
        config.write().unwrap().sources.push(Source::Observer {
            id: "old".into(),
            database: legacy.path.clone(),
            blob_directory: Some(legacy.blobs.clone()),
            native_home: None,
        });
        refresh(&library);
        let page = wait(&library, 3);
        let rows = page["records"].as_array().unwrap();
        let unassigned = rows.iter().find(|r| r["title"] == "Synthetic A").unwrap();
        assert!(unassigned["projectId"].is_null());
        assert_eq!(unassigned["recordedCwd"], "/synthetic/execution");
        assert_eq!(unassigned["projectBasis"], "legacy_recorded");
        let assigned = rows.iter().find(|r| r["title"] == "Synthetic B").unwrap();
        assert_eq!(assigned["projectPath"], "/synthetic/project");
        assert_eq!(assigned["recordedCwd"], "/synthetic/different");
        assert_eq!(
            (
                fs::read(&legacy.path).unwrap(),
                crate::history::files::signature(&fs::metadata(&legacy.path).unwrap())
            ),
            before
        );
        assert!(!legacy.path.with_extension("sqlite-wal").exists());
    }
}

#[test]
fn cold_old_workbench_backfill_uses_unassigned_native_cwd_after_all_sources_finish() {
    use crate::workbench::{
        live::{LiveHub, LiveLimits},
        recording::{Recorder, RecorderOptions},
    };
    for native_first in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("history");
        let home = temp.path().join("native");
        let other = temp.path().join("other-native");
        let cwd = temp.path().join("Documents/Codex/2026-09-23/new-chat");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&home).unwrap();
        let cwd = cwd.canonicalize().unwrap();
        fs::create_dir_all(&other).unwrap();
        let file = if native_first { &home } else { &other }.join("sessions/a.jsonl");
        rollout(&file, uuid::Uuid::new_v4(), &cwd, "native unassigned", "");
        let text = fs::read_to_string(&file)
            .unwrap()
            .replace("\"cwd\":", "\"originator\":\"Codex Desktop\",\"cwd\":");
        // Force native scanning to span several batches while the old run stages first.
        fs::write(&file, format!("{text}{}", " \n".repeat(3 * 1024 * 1024))).unwrap();
        let before = crate::history::files::signature(&fs::metadata(&file).unwrap());
        let hub = LiveHub::new(LiveLimits::default());
        let recorder = Recorder::start(
            &hub,
            RecorderOptions::new(data.clone(), &cwd, "old run".into()),
        )
        .unwrap();
        drop(recorder);
        let meta = data
            .join("runs")
            .join(hub.epoch().to_string())
            .join("meta.json");
        let deadline = Instant::now() + Duration::from_secs(4);
        while !meta.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        let config = Arc::new(RwLock::new(LibraryConfig {
            sources: vec![Source::Native {
                id: "later".into(),
                codex_home: other,
            }],
            ..Default::default()
        }));
        let policy = config.clone();
        let library = HistoryLibrary::with_policy(
            home,
            data,
            Arc::new(move || Ok(policy.read().unwrap().clone())),
        )
        .unwrap();
        let first = wait(&library, 2);
        let rows = first["records"].as_array().unwrap();
        assert!(rows.iter().find(|r| r["kind"] == "native").unwrap()["projectId"].is_null());
        let old = rows.iter().find(|r| r["kind"] == "workbench").unwrap();
        assert_eq!(old["projectPath"], cwd.to_str().unwrap());
        assert_eq!(old["recordedCwd"], cwd.to_str().unwrap());
        assert_eq!(old["projectBasis"], "cwd_inferred");
        refresh(&library);
        let refreshed = wait(&library, 2);
        assert_eq!(refreshed["records"], first["records"]);
        assert_eq!(refreshed["revision"], first["revision"]);
        assert_eq!(
            crate::history::files::signature(&fs::metadata(&file).unwrap()),
            before
        );
    }
}

#[test]
fn parent_navigation_only_links_existing_same_source_records_in_lists_and_windows() {
    let (_temp, home, _data, _config, library) = fixture();
    let parent = uuid::Uuid::new_v4();
    let child = uuid::Uuid::new_v4();
    let file = home.join("sessions/child.jsonl");
    rollout(&file, child, Path::new("/synthetic"), "child", "");
    let text = fs::read_to_string(&file).unwrap().replace(
        "\"cwd\":",
        &format!("\"parent_thread_id\":\"{parent}\",\"cwd\":"),
    );
    fs::write(&file, text).unwrap();
    refresh(&library);
    let page = wait(&library, 1);
    let id = page["records"][0]["entryId"].as_str().unwrap().to_string();
    assert!(page["records"][0]["parentEntryId"].is_null());
    for window in [false, true] {
        assert!(
            query(
                &library.handle.shared,
                Operation::Body(
                    id.clone(),
                    Query {
                        window,
                        ..Default::default()
                    }
                )
            )
            .unwrap()["entry"]["parentEntryId"]
                .is_null()
        );
    }
    rollout(
        &home.join("sessions/parent.jsonl"),
        parent,
        Path::new("/synthetic"),
        "parent",
        "",
    );
    refresh(&library);
    wait(&library, 2);
    for window in [false, true] {
        assert!(
            query(
                &library.handle.shared,
                Operation::Body(
                    id.clone(),
                    Query {
                        window,
                        ..Default::default()
                    }
                )
            )
            .unwrap()["entry"]["parentEntryId"]
                .is_string()
        );
    }
}
#[test]
fn project_navigation_orders_recent_records_and_keeps_unassigned_out_of_pages() {
    let (_temp, home, _data, _config, library) = fixture();
    for (name, cwd, time) in [
        ("a", "/synthetic/older", "2026-09-01T00:00:00Z"),
        ("b", "/synthetic/newer", "2026-09-23T00:00:00Z"),
        ("c", "", "2026-09-24T00:00:00Z"),
    ] {
        rollout(
            &home.join(format!("sessions/{name}.jsonl")),
            uuid::Uuid::new_v4(),
            Path::new(cwd),
            "navigation marker",
            &format!(
                "{}\n",
                json!({"type":"turn_context","timestamp":time,"payload":{}})
            ),
        );
    }
    refresh(&library);
    wait(&library, 3);
    let first = query(
        &library.handle.shared,
        Operation::List(
            Query {
                limit: Some(1),
                ..Default::default()
            },
            true,
        ),
    )
    .unwrap();
    assert_eq!(first["unassignedRecords"], 1);
    assert_eq!(first["records"][0]["path"], "/synthetic/newer");
    assert_eq!(
        first["records"][0]["latestRecordedAt"],
        "2026-09-23T00:00:00.000Z"
    );
    let second = query(
        &library.handle.shared,
        Operation::List(
            Query {
                limit: Some(1),
                cursor: first["nextCursor"].as_str().map(String::from),
                ..Default::default()
            },
            true,
        ),
    )
    .unwrap();
    assert_eq!(second["records"][0]["path"], "/synthetic/older");
    assert_eq!(second["unassignedRecords"], 1);
    assert!(second["nextCursor"].is_null());
    let valid = query(
        &library.handle.shared,
        Operation::List(
            Query {
                project_id: first["records"][0]["projectId"].as_str().map(String::from),
                q: Some("no match".into()),
                ..Default::default()
            },
            false,
        ),
    )
    .unwrap();
    assert_eq!(valid["projectExists"], true);
    assert!(valid["records"].as_array().unwrap().is_empty());
    let obsolete = query(
        &library.handle.shared,
        Operation::List(
            Query {
                project_id: Some("p_removed".into()),
                ..Default::default()
            },
            false,
        ),
    )
    .unwrap();
    assert_eq!(obsolete["projectExists"], false);
}

#[test]
fn project_navigation_keeps_equal_and_unknown_times_stable_across_pages() {
    let (_temp, home, _data, _config, library) = fixture();
    fs::create_dir_all(home.join("sessions")).unwrap();
    for (name, timestamp) in [
        ("b", Some("2026-09-23T00:00:00Z")),
        ("a", Some("2026-09-23T00:00:00Z")),
        ("unknown", None),
    ] {
        fs::write(home.join(format!("sessions/{name}.jsonl")), format!("{}\n", json!({"type":"session_meta","payload":{"id":uuid::Uuid::new_v4(),"cwd":format!("/synthetic/{name}"),"timestamp":timestamp}}))).unwrap();
    }
    refresh(&library);
    wait(&library, 3);
    let mut cursor = None;
    let mut paths = vec![];
    loop {
        let page = query(
            &library.handle.shared,
            Operation::List(
                Query {
                    limit: Some(1),
                    cursor,
                    ..Default::default()
                },
                true,
            ),
        )
        .unwrap();
        paths.push(page["records"][0]["path"].as_str().unwrap().to_string());
        if paths.len() == 3 {
            assert!(page["records"][0]["latestRecordedAt"].is_null());
        }
        cursor = page["nextCursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        paths,
        ["/synthetic/a", "/synthetic/b", "/synthetic/unknown"]
    );
}
#[test]
fn native_identity_archive_zstd_partial_unknown_and_no_source_writes() {
    let (_temp, home, _data, _config, library) = fixture();
    let id = uuid::Uuid::new_v4();
    let file = home.join("sessions/a.jsonl");
    rollout(
        &file,
        id,
        Path::new("/synthetic/project"),
        "你好 catalog",
        "{\"type\":\"future\",\"payload\":{}}\n{\"partial\":",
    );
    let before = fs::read(&file).unwrap();
    let metadata = crate::history::files::signature(&fs::metadata(&file).unwrap());
    refresh(&library);
    let page = wait(&library, 1);
    let entry = &page["records"][0];
    assert_eq!(entry["nativeThreadId"], id.to_string());
    assert_eq!(entry["capabilities"]["manage"], false);
    assert!(
        entry["coverage"]["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("partial_tail"))
    );
    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(
        crate::history::files::signature(&fs::metadata(&file).unwrap()),
        metadata
    );
    let stable = entry["entryId"].clone();
    fs::create_dir(home.join("archived_sessions")).unwrap();
    let archived = home.join("archived_sessions/renamed.jsonl.zst");
    fs::write(&archived, zstd::stream::encode_all(&before[..], 1).unwrap()).unwrap();
    fs::remove_file(&file).unwrap();
    refresh(&library);
    assert_eq!(wait(&library, 1)["records"][0]["entryId"], stable);
    let search = query(
        &library.handle.shared,
        Operation::List(
            Query {
                q: Some("你好".into()),
                ..Default::default()
            },
            false,
        ),
    )
    .unwrap();
    assert_eq!(search["records"].as_array().unwrap().len(), 1);
    let body = query(
        &library.handle.shared,
        Operation::Body(stable.as_str().unwrap().into(), Query::default()),
    )
    .unwrap();
    assert!(body.to_string().contains("unknown"));
}
#[test]
fn sources_never_merge_same_uuid_and_revocation_is_immediate() {
    let (temp, home, _data, config, library) = fixture();
    let id = uuid::Uuid::new_v4();
    rollout(
        &home.join("sessions/a.jsonl"),
        id,
        Path::new("/first/same"),
        "first",
        "",
    );
    let other = temp.path().join("other");
    rollout(
        &other.join("sessions/a.jsonl"),
        id,
        Path::new("/second/same"),
        "second",
        "",
    );
    config.write().unwrap().sources.push(Source::Native {
        id: "other".into(),
        codex_home: other,
    });
    refresh(&library);
    let all = wait(&library, 2);
    let rows = all["records"].as_array().unwrap();
    assert_ne!(rows[0]["entryId"], rows[1]["entryId"]);
    assert_ne!(rows[0]["projectId"], rows[1]["projectId"]);
    let removed = rows.iter().find(|r| r["sourceId"] == "other").unwrap()["entryId"]
        .as_str()
        .unwrap()
        .to_string();
    config.write().unwrap().sources.clear();
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::Body(removed, Query::default())
        )
        .unwrap_err(),
        "entry_unavailable"
    );
    config.write().unwrap().enabled = false;
    assert!(
        query(
            &library.handle.shared,
            Operation::List(Query::default(), false)
        )
        .unwrap()["records"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn cursor_binds_query_generation_and_cannot_be_forged() {
    let (_temp, home, _data, _config, library) = fixture();
    for i in 0..3 {
        rollout(
            &home.join(format!("sessions/{i}.jsonl")),
            uuid::Uuid::new_v4(),
            Path::new("/synthetic"),
            "hello",
            "",
        );
    }
    refresh(&library);
    wait(&library, 3);
    let first = query(
        &library.handle.shared,
        Operation::List(
            Query {
                limit: Some(1),
                ..Default::default()
            },
            false,
        ),
    )
    .unwrap();
    let cursor = first["nextCursor"].as_str().unwrap().to_string();
    let second = query(
        &library.handle.shared,
        Operation::List(
            Query {
                limit: Some(1),
                cursor: Some(cursor.clone()),
                ..Default::default()
            },
            false,
        ),
    )
    .unwrap();
    assert_ne!(
        first["records"][0]["entryId"],
        second["records"][0]["entryId"]
    );
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::List(
                Query {
                    limit: Some(1),
                    cursor: Some(cursor.clone()),
                    q: Some("changed".into()),
                    ..Default::default()
                },
                false
            )
        )
        .unwrap_err(),
        "stale_cursor"
    );
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::List(
                Query {
                    cursor: Some("fake".into()),
                    ..Default::default()
                },
                false
            )
        )
        .unwrap_err(),
        "invalid_cursor"
    );
    refresh(&library);
    wait(&library, 3);
    // An unchanged refresh keeps pagination valid.
    assert!(
        query(
            &library.handle.shared,
            Operation::List(
                Query {
                    limit: Some(1),
                    cursor: Some(cursor.clone()),
                    ..Default::default()
                },
                false
            )
        )
        .is_ok()
    );
    rollout(
        &home.join("sessions/new.jsonl"),
        uuid::Uuid::new_v4(),
        Path::new("/synthetic"),
        "added",
        "",
    );
    refresh(&library);
    wait(&library, 4);
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::List(
                Query {
                    limit: Some(1),
                    cursor: Some(cursor),
                    ..Default::default()
                },
                false
            )
        )
        .unwrap_err(),
        "stale_cursor"
    );
}
#[test]
fn source_symlinks_never_read_and_bad_line_does_not_stop_later_records() {
    let (temp, home, _data, _config, library) = fixture();
    let outside = temp.path().join("secret.jsonl");
    rollout(
        &outside,
        uuid::Uuid::new_v4(),
        Path::new("/outside"),
        "outside-secret",
        "",
    );
    fs::create_dir(home.join("sessions")).unwrap();
    symlink(&outside, home.join("sessions/link.jsonl")).unwrap();
    let file = home.join("sessions/valid.jsonl");
    rollout(
        &file,
        uuid::Uuid::new_v4(),
        Path::new("/synthetic"),
        "hello",
        "invalid\n{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"text\":\"after-invalid\"}]}}\n",
    );
    refresh(&library);
    let all = wait(&library, 1);
    assert!(all.to_string().contains("invalid_json_line"));
    let body = query(
        &library.handle.shared,
        Operation::Body(
            all["records"][0]["entryId"].as_str().unwrap().into(),
            Query::default(),
        ),
    )
    .unwrap();
    assert!(body.to_string().contains("after-invalid"));
    assert!(!body.to_string().contains("outside-secret"));
}
#[test]
fn settings_validation_and_explicit_preview_import_only_paths() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("observer.toml");
    fs::write(&path,"[server]\nbearer_token_file='never-import'\n[storage]\ndatabase='old.sqlite'\nblob_dir='blobs'\nfingerprint_key_file='secret'\n[[sources]]\ncodex_home='native'\n").unwrap();
    let source = scan::preview(path).unwrap();
    let json = json!(source);
    assert!(json["database"].as_str().unwrap().ends_with("old.sqlite"));
    assert!(!json.to_string().contains("secret"));
    assert!(!json.to_string().contains("bearer"));
    let mut config = LibraryConfig::default();
    config.sources.push(source.clone());
    config.sources.push(source);
    assert_eq!(config.validate().unwrap_err(), "invalid_source_id");
    assert!(
        serde_json::from_value::<Source>(
            json!({"kind":"native","id":"a","codexHome":"/test","password":"secret"})
        )
        .is_err()
    );
}

#[test]
fn three_source_kinds_schema20_and25_read_together_without_writes() {
    use crate::workbench::{
        live::{LiveHub, LiveLimits},
        recording::{Recorder, RecorderOptions, library::Project},
    };
    let legacy20 = crate::history::tests::Fixture::new(20);
    let legacy25 = crate::history::tests::Fixture::new(25);
    let before20 = fs::read(&legacy20.path).unwrap();
    let before25 = fs::read(&legacy25.path).unwrap();
    let (temp, home, data, config, library) = fixture();
    let cwd = temp.path().join("project");
    fs::create_dir(&cwd).unwrap();
    rollout(
        &home.join("sessions/synthetic.jsonl"),
        uuid::Uuid::new_v4(),
        &cwd,
        "native hello",
        "",
    );
    let hub = LiveHub::new(LiveLimits::default());
    let mut options = RecorderOptions::new(data.clone(), &cwd, "test workbench".into());
    options.project = Some(Project {
        format_version: 1,
        workspace_id: options.workspace_id.clone(),
        cwd: cwd.to_string_lossy().into(),
        native_home: home.to_string_lossy().into(),
    });
    let recorder = Recorder::start(&hub, options).unwrap();
    drop(recorder);
    let run = data.join("runs").join(hub.epoch().to_string());
    let deadline = Instant::now() + Duration::from_secs(4);
    while !run.join("project.json").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    config.write().unwrap().sources = vec![
        Source::Observer {
            id: "old20".into(),
            database: legacy20.path.clone(),
            blob_directory: Some(legacy20.blobs.clone()),
            native_home: None,
        },
        Source::Observer {
            id: "old25".into(),
            database: legacy25.path.clone(),
            blob_directory: Some(legacy25.blobs.clone()),
            native_home: None,
        },
    ];
    let run_bytes = fs::read(run.join("meta.json")).unwrap();
    refresh(&library);
    let all = wait(&library, 8);
    let entries = all["records"].as_array().unwrap();
    assert_eq!(entries.iter().filter(|e| e["kind"] == "native").count(), 1);
    assert_eq!(
        entries.iter().filter(|e| e["kind"] == "workbench").count(),
        1
    );
    assert_eq!(
        entries.iter().filter(|e| e["kind"] == "observer").count(),
        6
    );
    let workbench = entries.iter().find(|e| e["kind"] == "workbench").unwrap();
    assert_eq!(
        workbench["projectPath"],
        cwd.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    let body = query(
        &library.handle.shared,
        Operation::Body(
            workbench["entryId"].as_str().unwrap().into(),
            Query::default(),
        ),
    )
    .unwrap();
    assert!(!body["records"].as_array().unwrap().is_empty());
    let search = query(
        &library.handle.shared,
        Operation::List(
            Query {
                q: Some("Hello".into()),
                kind: Some("observer".into()),
                ..Default::default()
            },
            false,
        ),
    )
    .unwrap();
    assert_eq!(search["records"].as_array().unwrap().len(), 2);
    for entry in entries {
        let window = query(
            &library.handle.shared,
            Operation::Body(
                entry["entryId"].as_str().unwrap().into(),
                Query {
                    window: true,
                    source_revision: entry["sourceRevision"].as_str().map(String::from),
                    ..Default::default()
                },
            ),
        )
        .unwrap();
        assert_eq!(window["entry"]["sourceId"], entry["sourceId"]);
        if let Some(record) = window["records"].as_array().unwrap().first() {
            let detail = query(
                &library.handle.shared,
                Operation::Details(
                    entry["entryId"].as_str().unwrap().into(),
                    Query {
                        window: true,
                        cursor: record["detailCursor"].as_str().map(String::from),
                        ..Default::default()
                    },
                ),
            )
            .unwrap();
            assert!(!detail.to_string().contains("fixture-secret"));
        }
    }
    assert_eq!(fs::read(&legacy20.path).unwrap(), before20);
    assert_eq!(fs::read(&legacy25.path).unwrap(), before25);
    assert_eq!(fs::read(run.join("meta.json")).unwrap(), run_bytes);
    let meta: Value = serde_json::from_slice(&run_bytes).unwrap();
    let log = run.join(format!(
        "observations.{}.jsonl",
        meta["segments"][0]["id"].as_str().unwrap()
    ));
    let log_bytes = fs::read(&log).unwrap();
    fs::write(&log, &log_bytes).unwrap();
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::Body(
                workbench["entryId"].as_str().unwrap().into(),
                Query {
                    window: true,
                    source_revision: workbench["sourceRevision"].as_str().map(String::from),
                    ..Default::default()
                }
            )
        )
        .unwrap_err(),
        "source_revision_changed"
    );

    refresh(&library);
    assert_eq!(wait(&library, 8)["records"].as_array().unwrap().len(), 8);
}
#[test]
fn warm_cache_single_writer_and_corrupt_derived_index_rebuild() {
    let (_temp, home, data, config, library) = fixture();
    rollout(
        &home.join("sessions/a.jsonl"),
        uuid::Uuid::new_v4(),
        Path::new("/synthetic"),
        "cached",
        "",
    );
    refresh(&library);
    let first = wait(&library, 1);
    let policy = config.clone();
    let second = HistoryLibrary::with_policy(
        home.clone(),
        data.clone(),
        Arc::new(move || Ok(policy.read().unwrap().clone())),
    )
    .unwrap();
    assert_eq!(
        query(
            &second.handle.shared,
            Operation::List(Query::default(), false)
        )
        .unwrap()["records"],
        first["records"]
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        second
            .handle
            .statuses()
            .iter()
            .any(|s| s.state == "read_only")
    );
    drop(second);
    drop(library);
    fs::write(
        data.join("library/catalog.sqlite"),
        b"corrupted derived cache",
    )
    .unwrap();
    let policy = config.clone();
    let rebuilt = HistoryLibrary::with_policy(
        home,
        data,
        Arc::new(move || Ok(policy.read().unwrap().clone())),
    )
    .unwrap();
    assert_eq!(wait(&rebuilt, 1)["records"], first["records"]);
}
#[test]
fn native_replacement_oversize_and_secret_redaction_remain_explicit() {
    let (_temp, home, _data, _config, library) = fixture();
    let file = home.join("sessions/a.jsonl");
    let first = uuid::Uuid::new_v4();
    rollout(
        &file,
        first,
        Path::new("/synthetic"),
        "Authorization: Bearer synthetic-secret",
        "",
    );
    use std::io::Write;
    let mut append = fs::OpenOptions::new().append(true).open(&file).unwrap();
    writeln!(append, "{}", "x".repeat(1024 * 1024 + 1)).unwrap();
    writeln!(append,"{}",json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"text":"readable after oversize"}]}})).unwrap();
    drop(append);
    refresh(&library);
    let before = wait(&library, 1);
    assert!(before.to_string().contains("line_limit"));
    assert!(!before.to_string().contains("synthetic-secret"));
    let id = before["records"][0]["entryId"]
        .as_str()
        .unwrap()
        .to_string();
    let body = query(
        &library.handle.shared,
        Operation::Body(id.clone(), Query::default()),
    )
    .unwrap();
    assert!(body.to_string().contains("readable after oversize"));
    assert!(!body.to_string().contains("synthetic-secret"));
    let replacement = uuid::Uuid::new_v4();
    rollout(&file, replacement, Path::new("/synthetic"), "replaced", "");
    refresh(&library);
    let after = wait(&library, 1);
    assert_eq!(
        after["records"][0]["nativeThreadId"],
        replacement.to_string()
    );
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::Body(id, Query::default())
        )
        .unwrap_err(),
        "entry_unavailable"
    );
}

#[test]
fn cache_body_and_response_budgets_have_explicit_coverage_and_checkpoint_reuse() {
    let (_temp, home, data, _config, library) = fixture();
    let file = home.join("sessions/large.jsonl");
    rollout(
        &file,
        uuid::Uuid::new_v4(),
        Path::new("/synthetic"),
        "budget fixture",
        "",
    );
    use std::io::Write;
    let mut output = fs::OpenOptions::new().append(true).open(&file).unwrap();
    for _ in 0..90 {
        writeln!(output,"{}",json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"text":"x".repeat(30000)}]}})).unwrap();
    }
    drop(output);
    refresh(&library);
    let entries = wait(&library, 1);
    let entry = &entries["records"][0];
    assert!(entry.to_string().contains("body_cache_limit"));
    let first = query(
        &library.handle.shared,
        Operation::Body(
            entry["entryId"].as_str().unwrap().into(),
            Query {
                limit: Some(200),
                ..Default::default()
            },
        ),
    )
    .unwrap();
    assert!(first.to_string().len() < 1024 * 1024);
    assert!(first["nextCursor"].is_string());
    assert_eq!(first["coverage"]["state"], "partial");
    assert_eq!(first["entry"]["entryId"], entry["entryId"]);
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::Body(
                entry["entryId"].as_str().unwrap().into(),
                Query {
                    source_revision: Some("old-revision".into()),
                    ..Default::default()
                }
            )
        )
        .unwrap_err(),
        "source_revision_changed"
    );
    let db = Connection::open_with_flags(
        data.join("library/catalog.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM catalog_checkpoints c JOIN catalog_sources s ON s.generation=c.generation",[],|r|r.get::<_,u64>(0)).unwrap(),1);
    drop(db);
    refresh(&library);
    assert_eq!(wait(&library, 1)["records"], entries["records"]);
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::Details(entry["entryId"].as_str().unwrap().into(), Query::default())
        )
        .unwrap_err(),
        "details_unavailable"
    );
}
#[test]
fn interrupted_generation_never_replaces_committed_rows_and_cold_query_does_not_scan() {
    let (_temp, home, data, config, library) = fixture();
    rollout(
        &home.join("sessions/a.jsonl"),
        uuid::Uuid::new_v4(),
        Path::new("/synthetic"),
        "committed",
        "",
    );
    refresh(&library);
    let page = wait(&library, 1);
    drop(library);
    let db = Connection::open(data.join("library/catalog.sqlite")).unwrap();
    db.execute("INSERT INTO catalog_entries SELECT 'aborted',id,source,project,kind,time,metadata,search FROM catalog_entries LIMIT 1",[]).unwrap();
    drop(db);
    let policy = config.clone();
    let shared = Shared {
        home: home.clone(),
        data: data.clone(),
        policy: Arc::new(move || Ok(policy.read().unwrap().clone())),
        status: RwLock::new(vec![]),
        stop: AtomicBool::new(false),
        refresh: AtomicBool::new(false),
        key: [5; 32],
    };
    // Source changed after shutdown. A cache-only query cannot see the new file.
    rollout(
        &home.join("sessions/b.jsonl"),
        uuid::Uuid::new_v4(),
        Path::new("/synthetic"),
        "not scanned",
        "",
    );
    let start = Instant::now();
    let cached = query(&shared, Operation::List(Query::default(), false)).unwrap();
    assert_eq!(cached["records"], page["records"]);
    assert!(start.elapsed() < Duration::from_secs(1));
    let policy = config.clone();
    let restarted = HistoryLibrary::with_policy(
        home,
        data,
        Arc::new(move || Ok(policy.read().unwrap().clone())),
    )
    .unwrap();
    wait(&restarted, 2);
}

#[test]
fn old_run_projects_require_exact_workspace_hash_and_invalid_sidecars_are_ignored() {
    use crate::workbench::{
        live::{LiveHub, LiveLimits},
        recording::{Recorder, RecorderOptions},
    };
    let (temp, home, data, _config, library) = fixture();
    let cwd = temp.path().join("same-name");
    fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    rollout(
        &home.join("sessions/trusted.jsonl"),
        uuid::Uuid::new_v4(),
        &cwd,
        "trusted project",
        "",
    );
    refresh(&library);
    wait(&library, 1);
    let mut runs = vec![];
    for path in [cwd.clone(), PathBuf::from("/unrecorded/same-name")] {
        let hub = LiveHub::new(LiveLimits::default());
        let recorder = Recorder::start(
            &hub,
            RecorderOptions::new(data.clone(), &path, "same-name".into()),
        )
        .unwrap();
        drop(recorder);
        runs.push(hub.epoch());
    }
    let deadline = Instant::now() + Duration::from_secs(4);
    while !data
        .join("runs")
        .join(runs[1].to_string())
        .join("meta.json")
        .exists()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let run = Directory::read_only(&data.join("runs").join(runs[1].to_string())).unwrap();
    run.atomic(
        "project.json",
        &serde_json::to_vec(
            &json!({"formatVersion":1,"workspaceId":"wrong","cwd":cwd,"nativeHome":home}),
        )
        .unwrap(),
    )
    .unwrap();
    refresh(&library);
    let all = wait(&library, 3);
    let entries = all["records"].as_array().unwrap();
    let matched = entries
        .iter()
        .find(|entry| entry["runId"] == runs[0].to_string())
        .unwrap();
    let unknown = entries
        .iter()
        .find(|entry| entry["runId"] == runs[1].to_string())
        .unwrap();
    assert_eq!(matched["projectPath"], cwd.to_string_lossy().as_ref());
    assert!(unknown["projectId"].is_null());
    assert!(unknown.to_string().contains("project_path_not_recorded"));
}
#[test]
fn explicit_observer_mapping_links_entries_without_combining_bodies() {
    let legacy = crate::history::tests::Fixture::new(25);
    let (_temp, home, _data, config, library) = fixture();
    let id = uuid::Uuid::new_v4();
    Connection::open(&legacy.path)
        .unwrap()
        .execute(
            "UPDATE threads SET codex_thread_id=? WHERE thread_key='thread-a'",
            [id.to_string()],
        )
        .unwrap();
    rollout(
        &home.join("sessions/native.jsonl"),
        id,
        Path::new("/synthetic/a"),
        "native independent",
        "",
    );
    config.write().unwrap().sources = vec![Source::Observer {
        id: "mapped".into(),
        database: legacy.path.clone(),
        blob_directory: Some(legacy.blobs.clone()),
        native_home: Some(home),
    }];
    refresh(&library);
    let all = wait(&library, 4);
    let entries = all["records"].as_array().unwrap();
    let native = entries.iter().find(|e| e["kind"] == "native").unwrap();
    let observer = entries
        .iter()
        .find(|e| e["kind"] == "observer" && e["nativeThreadId"] == id.to_string())
        .unwrap();
    assert_eq!(native["relatedEntryIds"], json!([observer["entryId"]]));
    assert_eq!(observer["relatedEntryIds"], json!([native["entryId"]]));
    let body = query(
        &library.handle.shared,
        Operation::Body(
            observer["entryId"].as_str().unwrap().into(),
            Query::default(),
        ),
    )
    .unwrap();
    assert!(!body.to_string().contains("native independent"));
    let details = query(
        &library.handle.shared,
        Operation::Details(
            observer["entryId"].as_str().unwrap().into(),
            Query::default(),
        ),
    )
    .unwrap();
    assert!(
        details["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["kind"] == "observer_instructions")
    );
}

#[test]
fn native_roles_context_typed_messages_and_repeated_turns_are_distinct() {
    let (_temp, home, _data, _config, library) = fixture();
    let id = uuid::Uuid::new_v4();
    let file = home.join("sessions/messages.jsonl");
    rollout(&file, id, Path::new("/synthetic"), "first", "");
    use std::io::Write;
    let mut output = fs::OpenOptions::new().append(true).open(&file).unwrap();
    for value in [
        json!({"type":"response_item","payload":{"type":"message","role":"developer","content":[{"text":"instructions are context"}]}}),
        json!({"type":"event_msg","payload":{"type":"item_completed","thread_id":id,"item":{"type":"UserMessage","id":"u1","content":[{"type":"text","text":"typed input"}]}}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"typed input"}]}}),
        json!({"type":"turn_context","payload":{}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"typed input"}]}}),
    ] {
        writeln!(output, "{value}").unwrap();
    }
    drop(output);
    refresh(&library);
    let page = wait(&library, 1);
    let body = query(
        &library.handle.shared,
        Operation::Body(
            page["records"][0]["entryId"].as_str().unwrap().into(),
            Query::default(),
        ),
    )
    .unwrap();
    let records = body["records"].as_array().unwrap();
    assert!(
        records
            .iter()
            .any(|r| r["kind"] == "context" && r["text"] == "instructions are context")
    );
    assert_eq!(
        records
            .iter()
            .filter(|r| r["kind"] == "user_message" && r["text"] == "typed input")
            .count(),
        2
    );
}

#[test]
fn source_windows_read_past_cache_and_pin_search_details_and_replacements() {
    let (_temp, home, _data, _config, library) = fixture();
    let id = uuid::Uuid::new_v4();
    let path = home.join("sessions/long.jsonl");
    rollout(
        &path,
        id,
        Path::new("/synthetic/window"),
        "window title",
        "",
    );
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    for i in 0..90 {
        writeln!(file,"{}",json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"text":format!("body {i} {}", "x".repeat(30000))}]}})).unwrap();
    }
    writeln!(file,"{}",json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"text":"AFTER_CACHE_END <script>unsafe</script> Authorization: Bearer fake-secret"}]}})).unwrap();
    drop(file);
    refresh(&library);
    let list = wait(&library, 1);
    let entry = &list["records"][0];
    let id = entry["entryId"].as_str().unwrap();
    let rev = entry["sourceRevision"].as_str().unwrap();
    assert!(
        entry["coverage"]["reasons"]
            .to_string()
            .contains("body_cache_limit")
    );
    let before = fs::read(&path).unwrap();
    let read = |cursor: Option<String>, details: bool| {
        query(
            &library.handle.shared,
            if details {
                Operation::Details(
                    id.into(),
                    Query {
                        window: true,
                        source_revision: Some(rev.into()),
                        cursor,
                        ..Default::default()
                    },
                )
            } else {
                Operation::Body(
                    id.into(),
                    Query {
                        window: true,
                        source_revision: Some(rev.into()),
                        cursor,
                        limit: Some(8),
                        ..Default::default()
                    },
                )
            },
        )
    };
    let first = read(None, false).unwrap();
    assert!(first["records"][0].get("raw").is_none());
    let token = first["records"][0]["detailCursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let detail = read(Some(token.clone()), true).unwrap();
    assert!(detail["records"][0]["raw"].is_object());
    assert_eq!(
        read(Some(token), false).unwrap_err(),
        "source_revision_changed"
    );
    let mut cursor = first["nextCursor"].as_str().map(String::from);
    let mut found = false;
    let mut pages = 0;
    while let Some(next) = cursor {
        let page = read(Some(next), false).unwrap();
        assert!(page.to_string().len() < 1024 * 1024);
        assert!(page["records"].as_array().unwrap().len() <= 8);
        found |= page.to_string().contains("AFTER_CACHE_END");
        assert!(!page.to_string().contains("fake-secret"));
        cursor = page["nextCursor"].as_str().map(String::from);
        pages += 1;
        assert!(pages < 30);
    }
    assert!(found);
    assert_eq!(fs::read(&path).unwrap(), before);
    let last = first["nextCursor"].as_str().unwrap().to_owned();
    fs::write(&path, b"replaced\n").unwrap();
    assert_eq!(
        read(Some(last), false).unwrap_err(),
        "source_revision_changed"
    );
}
#[test]
fn source_filters_search_positions_subagents_and_cursor_revocation() {
    let (_temp, home, _data, config, library) = fixture();
    let parent = uuid::Uuid::new_v4();
    let child = uuid::Uuid::new_v4();
    let path = home.join("sessions/agent.jsonl");
    rollout(
        &path,
        child,
        Path::new("/synthetic/project"),
        "needle hit",
        "",
    );
    let content=fs::read_to_string(&path).unwrap().replace("\"timestamp\":\"2026-09-22T01:00:00Z\"",&format!("\"source\":{{\"subagent\":{{\"thread_spawn\":{{\"parent_thread_id\":\"{parent}\",\"agent_nickname\":\"Agent A\",\"depth\":1}}}}}},\"timestamp\":\"2026-09-22T01:00:00Z\""));
    fs::write(&path, content).unwrap();
    refresh(&library);
    wait(&library, 1);
    let search = query(
        &library.handle.shared,
        Operation::List(
            Query {
                source_id: Some("default-native".into()),
                group: Some("agents".into()),
                q: Some("needle".into()),
                ..Default::default()
            },
            false,
        ),
    )
    .unwrap();
    let entry = &search["records"][0];
    assert_eq!(entry["parentThreadId"], parent.to_string());
    assert_eq!(entry["agentName"], "Agent A");
    assert_eq!(entry["match"]["sourceRevision"], entry["sourceRevision"]);
    let id = entry["entryId"].as_str().unwrap();
    let body = query(
        &library.handle.shared,
        Operation::Body(
            id.into(),
            Query {
                window: true,
                record: entry["match"]["record"].as_u64(),
                source_revision: entry["sourceRevision"].as_str().map(String::from),
                ..Default::default()
            },
        ),
    )
    .unwrap();
    assert!(
        body["records"][0]["text"]
            .as_str()
            .unwrap()
            .contains("needle")
    );
    assert!(
        query(
            &library.handle.shared,
            Operation::List(
                Query {
                    source_id: Some("default-workbench".into()),
                    ..Default::default()
                },
                false
            )
        )
        .unwrap()["records"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    config.write().unwrap().enabled = false;
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::Body(
                id.into(),
                Query {
                    window: true,
                    ..Default::default()
                }
            )
        )
        .unwrap_err(),
        "entry_unavailable"
    );
}

#[tokio::test]
#[ignore = "requires built codex-view and system Chrome; synthetic sources and isolated HOME"]
async fn binary_history_reading_chrome_all_sources_and_bounded_dom() {
    use crate::workbench::{
        decode::{Change, Decoded, TextKey},
        live::{LiveHub, LiveLimits},
        recording::{Recorder, RecorderOptions, library::Project},
        redaction::RedactionPolicy,
    };
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    let legacy = crate::history::tests::Fixture::new(25);
    let fixture_db = rusqlite::Connection::open(&legacy.path).unwrap();
    fixture_db
        .execute(
            "UPDATE threads SET project_key=cwd WHERE thread_key IN ('thread-a','thread-b')",
            [],
        )
        .unwrap();
    drop(fixture_db);
    let before_legacy = fs::read(&legacy.path).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("native");
    let data = temp.path().join("history");
    let cwd = temp.path().join("项目 A");
    fs::create_dir(&cwd).unwrap();
    let unassigned_cwd = temp.path().join("Documents/Codex/2026-09-23/new-chat");
    fs::create_dir_all(&unassigned_cwd).unwrap();
    let unassigned_path = home.join("sessions/unassigned.jsonl");
    rollout(
        &unassigned_path,
        uuid::Uuid::new_v4(),
        &unassigned_cwd,
        "未归属会话示例",
        "",
    );
    let unassigned_raw = fs::read_to_string(&unassigned_path).unwrap();
    let mut unassigned_lines = unassigned_raw.lines();
    let mut unassigned_meta: Value =
        serde_json::from_str(unassigned_lines.next().unwrap()).unwrap();
    unassigned_meta["payload"]["originator"] = json!("Codex Desktop");
    fs::write(
        &unassigned_path,
        format!(
            "{}\n{}\n",
            unassigned_meta,
            unassigned_lines.collect::<Vec<_>>().join("\n")
        ),
    )
    .unwrap();
    let before_unassigned = fs::read(&unassigned_path).unwrap();
    let parent = uuid::Uuid::new_v4();
    let path = home.join("sessions/reading.jsonl");
    rollout(&path, parent, &cwd, "优化文件阅读界面", "");
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    for i in 0..180 {
        writeln!(file,"{}",json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"text":format!("## 模型回复 {i}\n已完成 **文件阅读** 的布局调整。\n\n```rust\nfn main() {{ println!(\"安全阅读\"); }}\n```\n\n<img src=\"https://example.invalid/blocked\"><script>window.historyInjected=true</script>")}]}})).unwrap();
        writeln!(file,"{}",json!({"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo test\"}","call_id":format!("call-{i}")}})).unwrap();
    }
    drop(file);
    rollout(
        &home.join("sessions/child.jsonl"),
        uuid::Uuid::new_v4(),
        &cwd,
        "子代理检查",
        "",
    );
    let child = home.join("sessions/child.jsonl");
    let raw=fs::read_to_string(&child).unwrap().replace("\"timestamp\":\"2026-09-22T01:00:00Z\"",&format!("\"source\":{{\"subagent\":{{\"thread_spawn\":{{\"parent_thread_id\":\"{parent}\",\"depth\":1,\"agent_nickname\":\"检查员\"}}}}}},\"timestamp\":\"2026-09-22T01:00:00Z\""));
    fs::write(child, raw).unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let request = uuid::Uuid::new_v4();
    let policy = RedactionPolicy::new(vec![]).unwrap();
    hub.apply(Decoded {
        request_id: request,
        capture_seq: 1,
        received_at: Instant::now(),
        change: Change::TextReplace {
            key: TextKey {
                request_id: request,
                response_id: Some("synthetic-response".into()),
                wire_item_id: "message".into(),
                content_index: 0,
            },
            text: policy.scrub("工作台保存的模型正文"),
        },
    });
    let mut options = RecorderOptions::new(data.clone(), &cwd, "工作台示例".into());
    options.project = Some(Project {
        format_version: 1,
        workspace_id: options.workspace_id.clone(),
        cwd: cwd.to_string_lossy().into(),
        native_home: home.to_string_lossy().into(),
    });
    drop(Recorder::start(&hub, options).unwrap());
    let run = data.join("runs").join(hub.epoch().to_string());
    let until = Instant::now() + Duration::from_secs(5);
    while !run.join("meta.json").exists() || !run.join("project.json").exists() {
        assert!(Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    let before_run = fs::read(run.join("meta.json")).unwrap();
    let before_native = fs::read(&path).unwrap();
    let config_dir = temp.path().join("config");
    fs::create_dir(&config_dir).unwrap();
    fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = serde_json::to_value(crate::workbench::config::Config::default()).unwrap();
    config["history"]["library"]["sources"] = json!([{"id":"legacy-fixture","kind":"observer","database":legacy.path,"blobDirectory":legacy.blobs}]);
    fs::write(config_dir.join("config.json"), config.to_string()).unwrap();
    fs::set_permissions(
        config_dir.join("config.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let cli = temp.path().join("fake-codex");
    fs::write(
        &cli,
        "#!/bin/sh\nprintf invocation >> \"$CODEX_HOME/invocations\"\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGINT);
            };
            let _ = self.0.wait();
        }
    }
    let entry_path = temp.path().join("entry.json");
    let mut child = Child(
        Command::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/codex-view"))
            .current_dir(&cwd)
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("CODEX_HOME", &home)
            .arg("--no-open")
            .arg("--config-dir")
            .arg(&config_dir)
            .arg("--data-dir")
            .arg(&data)
            .arg("--codex-bin")
            .arg(&cli)
            .arg("--entry-file")
            .arg(&entry_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(10);
    let entry = loop {
        if let Ok(e) = crate::workbench::launch::read_entry(&entry_path) {
            break e;
        }
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let mut command = tokio::process::Command::new("node");
    command
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r6-reading-probe.cjs"))
        .env("WORKBENCH_PROBE_URL", &entry.url)
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("CODEX_HOME", &home)
        .env("DEBUG", "")
        .env("PWDEBUG", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("WORKBENCH_TEST_SCREENSHOT") {
        command.env("WORKBENCH_PROBE_SCREENSHOT", path);
    }
    let mut probe = crate::workbench::probe_process::ProbeProcess::spawn(&mut command).unwrap();
    let _live = probe.stdin.take();
    assert!(
        tokio::time::timeout(Duration::from_secs(60), probe.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!home.join("invocations").exists());
    assert_eq!(fs::read(&path).unwrap(), before_native);
    assert_eq!(fs::read(&unassigned_path).unwrap(), before_unassigned);
    assert_eq!(fs::read(&legacy.path).unwrap(), before_legacy);
    assert_eq!(fs::read(run.join("meta.json")).unwrap(), before_run);
    drop(child);
    assert!(!entry_path.exists());
}

#[test]
fn compressed_windows_keep_mirror_state_and_detail_cursor_does_not_cross_entries() {
    let (_temp, home, _data, _config, library) = fixture();
    let raw = home.join("sessions/mirror.jsonl");
    let id = uuid::Uuid::new_v4();
    rollout(
        &raw,
        id,
        Path::new("/synthetic"),
        "mirror message",
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"mirror message\"}}\n{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"exec_command\",\"arguments\":\"{\\\"cmd\\\":\\\"echo visible-command\\\"}\"}}\n",
    );
    let before = fs::read(&raw).unwrap();
    fs::create_dir(home.join("archived_sessions")).unwrap();
    fs::write(
        home.join("archived_sessions/mirror.jsonl.zst"),
        zstd::stream::encode_all(before.as_slice(), 1).unwrap(),
    )
    .unwrap();
    fs::remove_file(raw).unwrap();
    rollout(
        &home.join("sessions/other.jsonl"),
        uuid::Uuid::new_v4(),
        Path::new("/synthetic"),
        "other",
        "",
    );
    refresh(&library);
    let entries = wait(&library, 2);
    let entry = entries["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["nativeThreadId"] == id.to_string())
        .unwrap();
    let id = entry["entryId"].as_str().unwrap();
    let mut cursor = None;
    let mut users = 0;
    let mut tool = false;
    let mut detail = None;
    for _ in 0..8 {
        let page = query(
            &library.handle.shared,
            Operation::Body(
                id.into(),
                Query {
                    window: true,
                    limit: Some(1),
                    cursor,
                    ..Default::default()
                },
            ),
        )
        .unwrap();
        for r in page["records"].as_array().unwrap() {
            users += usize::from(r["role"] == "user");
            tool |= r["text"]
                .as_str()
                .is_some_and(|t| t.contains("visible-command"));
            detail = r["detailCursor"].as_str().map(String::from);
        }
        cursor = page["nextCursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(users, 1);
    assert!(tool);
    let other = entries["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["entryId"] != id)
        .unwrap()["entryId"]
        .as_str()
        .unwrap();
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::Details(
                other.into(),
                Query {
                    window: true,
                    cursor: detail,
                    ..Default::default()
                }
            )
        )
        .unwrap_err(),
        "source_revision_changed"
    );
}
