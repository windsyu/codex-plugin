//! Storage faults against a bounded temporary derived cache, never the user's disk.
use super::*;

#[test]
fn sqlite_full_rolls_back_document_without_publishing_staging_or_changing_source() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("synthetic-native");
    let data = temp.path().join("derived-history");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    let file = home.join("sessions/synthetic.jsonl");
    let id = uuid::Uuid::new_v4();
    let bytes = format!(
        "{}\n{}\n",
        json!({"type":"session_meta","payload":{"id":id,"cwd":"/synthetic/project","timestamp":"2026-09-24T00:00:00Z"}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"synthetic durable source"}]}})
    );
    std::fs::write(&file, &bytes).unwrap();
    let source_signature = crate::history::files::signature(&std::fs::metadata(&file).unwrap());
    let shared = Shared {
        home: home.clone(),
        data,
        policy: Arc::new(|| Ok(LibraryConfig::default())),
        status: RwLock::new(vec![]),
        stop: AtomicBool::new(false),
        refresh: AtomicBool::new(false),
        key: [7; 32],
    };
    let source = Source::Native {
        id: "default-native".into(),
        codex_home: home,
    };
    let mut scan = Scan::open(source.clone()).unwrap();
    let document = loop {
        match scan.step(&shared.key).unwrap() {
            Step::Document(document) => break document,
            Step::Done => panic!("synthetic rollout must produce a document"),
            Step::More | Step::Checkpoint(_) => {}
        }
    };
    let (_directory, _writer_lock, mut db) = open_cache(&shared).unwrap();
    let written = write_document(
        &mut db,
        "committed",
        &document,
        Some("committed-file"),
        usize::MAX,
    )
    .unwrap();
    publish(
        &mut db,
        &shared,
        Job {
            scan,
            generation: "committed".into(),
            count: 1,
            bytes: written,
            partial: false,
            checkpoint: None,
            backfill_after: String::new(),
        },
    );
    let before = query(&shared, Operation::List(Query::default(), false)).unwrap();
    assert_eq!(before["records"].as_array().unwrap().len(), 1);
    let entry_id = document.entry.entry_id.clone();
    let before_body = query(&shared, Operation::Body(entry_id.clone(), Query::default())).unwrap();

    // One successfully staged document must stay invisible even when a later
    // document cannot be committed. This is more than an empty transaction test.
    let mut staged_entry = Entry::new(&source, "staged-only");
    staged_entry.title = "must not appear in published directory".into();
    let staged = Document {
        entry: staged_entry,
        records: vec![json!({"kind":"message","text":"unpublished"})],
        locator: json!({}),
    };
    write_document(&mut db, "staging", &staged, Some("staged-file"), usize::MAX).unwrap();
    let pages: u64 = db
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    let maximum: u64 = db
        .query_row(&format!("PRAGMA max_page_count={}", pages + 2), [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(maximum, pages + 2);
    let page_size: u64 = db
        .pragma_query_value(None, "page_size", |r| r.get(0))
        .unwrap();
    let oversized = (maximum * page_size + 64 * 1024) as usize;
    assert!(
        oversized < 2 * 1024 * 1024,
        "fault injection must remain small"
    );
    // Establish the actual SQLite failure code; the product deliberately maps
    // the same failure to the payload-free cache_write_failed diagnostic.
    let error = db
        .execute(
            "INSERT INTO catalog_records VALUES ('probe','full',0,zeroblob(?1))",
            [oversized as i64],
        )
        .unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DiskFull)
    );
    assert!(db.is_autocommit());
    let failed = Document {
        entry: Entry::new(&source, "failed-document"),
        records: vec![json!({"kind":"message","text":"x".repeat(oversized)})],
        locator: json!({"synthetic":true}),
    };
    assert_eq!(
        write_document(&mut db, "staging", &failed, Some("failed-file"), usize::MAX),
        Err("cache_write_failed")
    );
    assert!(
        db.is_autocommit(),
        "SQLITE_FULL must not leave an open transaction"
    );
    for table in [
        "catalog_entries",
        "catalog_records",
        "catalog_locators",
        "catalog_checkpoints",
        "catalog_cwds",
    ] {
        let key = if table == "catalog_entries" {
            "id"
        } else {
            "entry"
        };
        let count: u64 = db
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE generation='staging' AND {key}=?1"),
                [&failed.entry.entry_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "failed document left rows in {table}");
    }
    let staged_count: u64 = db
        .query_row(
            "SELECT count(*) FROM catalog_entries WHERE generation='staging'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(staged_count, 1);
    let generation: String = db
        .query_row(
            "SELECT generation FROM catalog_sources WHERE id='default-native'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(generation, "committed");
    assert_eq!(
        query(&shared, Operation::List(Query::default(), false)).unwrap()["records"],
        before["records"]
    );
    assert_eq!(
        query(&shared, Operation::Body(entry_id, Query::default())).unwrap(),
        before_body
    );
    assert_eq!(std::fs::read(&file).unwrap(), bytes.as_bytes());
    assert_eq!(
        crate::history::files::signature(&std::fs::metadata(&file).unwrap()),
        source_signature
    );
    assert_eq!(
        db.pragma_query_value(None, "integrity_check", |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    println!(
        "{}",
        json!({"check":"r6-g1-sqlite-full","initialPages":pages,"maxPages":maximum,"pageSize":page_size,"oversizedRecordBytes":oversized,"sqliteCode":"SQLITE_FULL","publishedGeneration":"committed","stagingEntriesInvisible":staged_count,"sourceUnchanged":true})
    );
}
