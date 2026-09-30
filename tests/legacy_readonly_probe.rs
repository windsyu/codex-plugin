//! Opt-in spot check of an explicitly selected legacy source using the product
//! reader. Output is aggregate only: no paths, IDs, cursors or private content.
#![cfg(unix)]

use codex_local_observer::history::legacy_reader::{Collection, LegacyReader, Query, ReadBudget};
use serde_json::{Value, json};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(PartialEq, Eq)]
struct Fingerprint {
    metadata: [u64; 10],
    hash: blake3::Hash,
}

fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path);
    let mut file = match file {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => panic!("cannot fingerprint source file"),
    };
    let metadata = |file: &File| {
        let m = file.metadata().expect("source file metadata");
        assert!(m.is_file() && m.nlink() == 1, "unsafe source file");
        [
            m.dev(),
            m.ino(),
            m.len(),
            m.mode() as u64,
            m.uid() as u64,
            m.gid() as u64,
            m.mtime() as u64,
            m.mtime_nsec() as u64,
            m.ctime() as u64,
            m.ctime_nsec() as u64,
        ]
    };
    let before = metadata(&file);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).expect("source fingerprint read");
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    assert!(before == metadata(&file), "source changed while hashing");
    Some(Fingerprint {
        metadata: before,
        hash: hasher.finalize(),
    })
}

fn source_files(path: &Path) -> Vec<PathBuf> {
    ["", "-wal", "-shm", "-journal"]
        .into_iter()
        .map(|suffix| {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            PathBuf::from(name)
        })
        .collect()
}

fn body_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.trim().len(),
        Value::Array(parts) => parts.iter().map(body_bytes).sum(),
        Value::Object(object) => {
            let text: usize = [
                "text",
                "message",
                "content",
                "input_text",
                "output_text",
                "output",
                "aggregated_output",
                "summary",
            ]
            .iter()
            .filter_map(|key| object.get(*key))
            .map(body_bytes)
            .sum();
            let wrapped: usize = ["payload", "item", "params"]
                .iter()
                .filter_map(|key| object.get(*key))
                .filter(|v| v.is_object())
                .map(body_bytes)
                .sum();
            text + wrapped
        }
        _ => 0,
    }
}

#[test]
fn body_evidence_excludes_ids_status_and_summary_only_records() {
    for metadata in [
        json!({"id":"a","type":"message","status":"completed"}),
        json!({"payload":{"id":"a"}}),
    ] {
        assert_eq!(body_bytes(&metadata), 0);
    }
    assert_eq!(body_bytes(&json!({"summaryText":"preview only"})), 0);
    assert!(
        body_bytes(
            &json!({"payload":{"content":[{"type":"output_text","text":"synthetic body"}]}})
        ) > 0
    );
}

fn sample(path: &Path, expected_schema: i64) -> Value {
    let reader = LegacyReader::open("g3-legacy", path, None, &ReadBudget::default())
        .expect("product read-only reader must open the selected source");
    assert_eq!(reader.manifest().schema_version, expected_schema);
    assert!(reader.manifest().validated_schema);
    let read = |collection, scope| {
        reader
            .page(
                &Query { collection, scope },
                None,
                20,
                &ReadBudget::default(),
            )
            .expect("bounded product reader query must succeed")
    };
    let threads = read(Collection::Threads, None);
    assert!(!threads.records.is_empty(), "source has no sampled threads");
    let revision = &threads.source_revision;
    let mut reports = Vec::new();
    let mut item_bodies = 0;
    let mut item_body_bytes = 0;
    for collection in [
        Collection::Projects,
        Collection::Sources,
        Collection::Epochs,
        Collection::Gaps,
    ] {
        let page = read(collection, None);
        assert!(
            page.source_revision == *revision,
            "source changed during probe"
        );
        reports.push(json!({
            "collection": collection, "queries": 1, "records": page.records.len(),
            "hasMore": page.next_cursor.is_some(), "pageIssues": page.issues.len(),
            "recordIssues": page.records.iter().map(|r| r.issues.len()).sum::<usize>(),
        }));
    }
    let sampled_threads = threads.records.len().min(10);
    for collection in [
        Collection::Context,
        Collection::Relations,
        Collection::Turns,
        Collection::Items,
        Collection::Raw,
    ] {
        let (mut count, mut record_issues, mut page_issues, mut more) = (0, 0, 0, 0);
        for thread in threads.records.iter().take(sampled_threads) {
            let key = thread.fields["threadKey"].as_str().expect("thread key");
            let page = read(collection, Some(key.to_owned()));
            assert!(
                page.source_revision == *revision,
                "source changed during probe"
            );
            count += page.records.len();
            page_issues += page.issues.len();
            record_issues += page.records.iter().map(|r| r.issues.len()).sum::<usize>();
            more += usize::from(page.next_cursor.is_some());
            if collection == Collection::Items {
                for record in &page.records {
                    let bytes = body_bytes(&record.fields["raw"]);
                    item_bodies += usize::from(bytes > 0);
                    item_body_bytes += bytes;
                }
            }
        }
        reports.push(json!({
            "collection": collection, "queries": sampled_threads, "records": count,
            "pagesWithMore": more, "pageIssues": page_issues, "recordIssues": record_issues,
        }));
    }
    assert!(
        item_bodies > 0,
        "metadata alone cannot pass the body spot check"
    );
    json!({
        "schemaVersion": reader.manifest().schema_version,
        "validatedSchema": reader.manifest().validated_schema,
        "threadRecords": threads.records.len(), "threadHasMore": threads.next_cursor.is_some(),
        "sampledThreads": sampled_threads, "itemBodies": item_bodies,
        "itemBodyBytes": item_body_bytes,
        "collections": reports, "attachmentsRead": false,
    })
}

#[test]
#[ignore = "explicit read-only legacy database spot check; requires WORKBENCH_TEST_LEGACY_DB and WORKBENCH_TEST_LEGACY_SCHEMA; aggregate output only"]
fn actual_legacy_database_is_readable_without_source_writes() {
    let path = PathBuf::from(
        std::env::var_os("WORKBENCH_TEST_LEGACY_DB").expect("explicit source required"),
    );
    let schema: i64 = std::env::var("WORKBENCH_TEST_LEGACY_SCHEMA")
        .expect("explicit expected schema required")
        .parse()
        .expect("numeric expected schema required");
    assert!(path.is_absolute(), "source must be absolute");
    assert!(matches!(schema, 20 | 25), "only validated legacy schemas");
    let files = source_files(&path);
    let before: Vec<_> = files.iter().map(|p| fingerprint(p)).collect();
    assert!(before[0].is_some(), "source must already exist");
    // Check source bytes even when a reader assertion fails. The reader is
    // dropped before the second snapshot; no SQLite writer is ever opened.
    let result = std::panic::catch_unwind(|| sample(&path, schema));
    let after: Vec<_> = files.iter().map(|p| fingerprint(p)).collect();
    assert!(
        before == after,
        "source changed; zero-write evidence is inconclusive"
    );
    let mut report = match result {
        Ok(report) => report,
        Err(error) => std::panic::resume_unwind(error),
    };
    report["sourceFilesUnchanged"] = json!(true);
    report["sourceFiles"] = json!(before.iter().flatten().count());
    report["sourceBytes"] = json!(before.iter().flatten().map(|f| f.metadata[2]).sum::<u64>());
    println!(
        "G3_LEGACY_READONLY {}",
        serde_json::to_string(&report).unwrap()
    );
}
