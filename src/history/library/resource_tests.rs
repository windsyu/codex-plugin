//! macOS-only opt-in resource evidence, using the actual live worker handles.
//! No instrumentation, counters or configuration are added to production builds.
use super::{scale_tests::*, *};
use std::{fs, io::Write, os::unix::thread::JoinHandleExt, path::Path};

fn thread_cpu(thread: &std::thread::JoinHandle<()>) -> Value {
    let mut info: libc::thread_basic_info = unsafe { std::mem::zeroed() };
    let mut count = libc::THREAD_BASIC_INFO_COUNT;
    // The library is borrowed and still owns both handles. Neither worker exits
    // until Drop sets stop; do not keep a Mach thread name after joining it.
    let result = unsafe {
        libc::thread_info(
            libc::pthread_mach_thread_np(thread.as_pthread_t()),
            libc::THREAD_BASIC_INFO as libc::thread_flavor_t,
            (&mut info as *mut libc::thread_basic_info).cast(),
            &mut count,
        )
    };
    assert_eq!(result, libc::KERN_SUCCESS, "worker CPU sampling failed");
    assert_eq!(count, libc::THREAD_BASIC_INFO_COUNT);
    let seconds =
        |time: libc::time_value_t| time.seconds as f64 + time.microseconds as f64 / 1_000_000.0;
    let user = seconds(info.user_time);
    let system = seconds(info.system_time);
    json!({"userSeconds":user,"systemSeconds":system,"totalSeconds":user+system})
}

pub(super) fn workers(library: &HistoryLibrary) -> Value {
    let sample = |name| {
        thread_cpu(
            library
                .threads
                .iter()
                .find(|t| t.thread().name() == Some(name))
                .expect("named history worker must be alive"),
        )
    };
    json!({"index":sample("history-library-index"),"query":sample("history-library-query"),"scope":"Mach THREAD_BASIC_INFO cumulative CPU for each library worker; excludes fixture generation and other threads"})
}

const SECRET: &str = "sk-g1-only-synthetic-secret-0123456789abcdef";
const SIZES: [usize; 4] = [512, 4096, 16384, 24576];
const SEEDS: [(&str, &str); 4] = [
    (
        "chinese",
        "这是合成历史正文，用于核对中文阅读、目录索引和搜索结果。每一条消息都保持可读，不包含图片。",
    ),
    (
        "english",
        "An ordinary readable explanation of the project: inspect requests, compare response content, preserve ordering, and document the result.\n",
    ),
    (
        "code",
        "```rust\nfn render_item(index: usize, text: &str) -> String {\n    format!(\"item {}: {}\", index, text)\n}\n```\n- Verify UTF-8: 文件与目录 ✅\n",
    ),
    (
        "credentials",
        "A synthetic diagnostic sample follows. Authorization: Bearer sk-g1-only-synthetic-secret-0123456789abcdef\nContinue reading the public explanation safely.\n",
    ),
];

fn message(seed: &str, session: usize, item: usize) -> String {
    format!(
        "g1text{session:04}item{item:02} {}",
        seed.repeat(SIZES[item % SIZES.len()].div_ceil(seed.len()))
    )
}

fn text_fixture(base: &Path, seed: &str, sessions: usize) -> Value {
    let home = base.join("native");
    fs::create_dir_all(home.join("sessions")).unwrap();
    fs::create_dir(base.join("project")).unwrap();
    let mut source_bytes = 0;
    let mut text_bytes = 0;
    for session in 0..sessions {
        let file = home.join("sessions").join(format!("s{session:04}.jsonl"));
        let mut out = std::io::BufWriter::new(fs::File::create(&file).unwrap());
        writeln!(out, "{}", json!({"type":"session_meta","payload":{"id":uuid::Uuid::from_u128(session as u128+1),"cwd":base.join("project"),"timestamp":"2026-09-30T00:00:00Z"}})).unwrap();
        for item in 0..20 {
            let text = message(seed, session, item);
            text_bytes += text.len();
            writeln!(out, "{}", json!({"type":"response_item","payload":{"type":"message","role":if item % 2 == 0 {"user"} else {"assistant"},"content":[{"type":"input_text","text":text}]}})).unwrap();
        }
        out.flush().unwrap();
        source_bytes += fs::metadata(file).unwrap().len();
    }
    json!({"sessions":sessions,"messages":sessions*20,"expectedRecords":sessions*21,"sourceBytes":source_bytes,"textBytes":text_bytes,"targetMessageBytes":SIZES,"images":0})
}

fn cached_counts(library: &HistoryLibrary) -> (usize, usize) {
    let db = read_db(&library.handle.shared).unwrap().unwrap();
    db.query_row("SELECT (SELECT count(*) FROM catalog_entries WHERE generation IN (SELECT generation FROM catalog_sources)), (SELECT count(*) FROM catalog_records WHERE generation IN (SELECT generation FROM catalog_sources))", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap()
}

fn verify_all_text(library: &HistoryLibrary, seed: &str) {
    let db = read_db(&library.handle.shared).unwrap().unwrap();
    let mut statement = db.prepare("SELECT e.metadata,r.seq,r.body FROM catalog_records r JOIN catalog_entries e ON e.generation=r.generation AND e.id=r.entry WHERE r.generation IN(SELECT generation FROM catalog_sources)").unwrap();
    let mut rows = statement.query([]).unwrap();
    let mut seen = std::collections::HashSet::new();
    while let Some(row) = rows.next().unwrap() {
        let metadata: Value = serde_json::from_str(&row.get::<_, String>(0).unwrap()).unwrap();
        assert_eq!(
            metadata["coverage"]["reasons"],
            json!(["search_excerpt_limit"])
        );
        let seq: usize = row.get(1).unwrap();
        if seq == 0 {
            continue;
        }
        assert!(seq <= 20);
        let session = uuid::Uuid::parse_str(metadata["nativeThreadId"].as_str().unwrap())
            .unwrap()
            .as_u128() as usize
            - 1;
        let record: Value = serde_json::from_str(&row.get::<_, String>(2).unwrap()).unwrap();
        assert_ne!(record["detailsOmitted"], true);
        // Independent expected masking for this known synthetic credential;
        // never use the production sanitizer as the assertion oracle.
        let expected =
            message(seed, session, seq - 1).replace(&format!("Bearer {SECRET}"), "[已脱敏]");
        assert_eq!(
            record["raw"]["payload"]["content"][0]["text"].as_str(),
            Some(expected.as_str()),
            "cached message {session}/{} differs",
            seq - 1
        );
        assert!(seen.insert((session, seq - 1)), "duplicate message");
    }
    assert_eq!(seen.len(), 32 * 20);
}

fn search_excerpt_only(page: &Value) {
    assert_eq!(page["coverage"]["state"], "partial");
    assert_eq!(
        page["coverage"]["reasons"],
        json!(["entry_coverage_partial", "source_partial:default-native"])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "G1 pure-text CPU/throughput matrix; synthetic data only, run alone"]
async fn g1_pure_text_worker_cpu_and_throughput() {
    for (name, seed) in SEEDS {
        for repeat in 0..3 {
            let temp = tempfile::tempdir().unwrap();
            let generated = Instant::now();
            let manifest = text_fixture(temp.path(), seed, 32);
            let generation_ms = generated.elapsed().as_secs_f64() * 1000.0;
            let before = resources();
            let started = Instant::now();
            let library = start(
                &temp.path().join("native"),
                &temp.path().join("history"),
                LibraryConfig::default(),
            );
            assert!(published(&library, 900).await);
            let wall = started.elapsed().as_secs_f64();
            let cpu = workers(&library);
            let after = resources();
            let counts = cached_counts(&library);
            let page = library.handle.list(Query::default(), false).await.unwrap();
            emit(
                "pure_text_publication",
                json!({"corpus":name,"repeat":repeat,"buildDebug":cfg!(debug_assertions),"manifest":manifest,"generationMs":generation_ms,"wallSeconds":wall,"textMiBPerWallSecond":manifest["textBytes"].as_f64().unwrap()/1048576.0/wall,"workers":cpu,"resourcesBefore":before,"resourcesAfter":after,"entries":counts.0,"records":counts.1,"coverage":page["coverage"],"derivedBytes":disk(&temp.path().join("history")),"scope":"new derived index, OS page cache not flushed; generation excluded; complete parse, redaction and cache publication"}),
            );
            assert_eq!(
                counts,
                (32, 32 * 21),
                "do not treat omitted bodies as throughput"
            );
            search_excerpt_only(&page);
            verify_all_text(&library, seed);
            assert!(cpu["index"]["totalSeconds"].as_f64().unwrap() > 0.0);
            let db = read_db(&library.handle.shared).unwrap().unwrap();
            let leaks: usize = db
                .query_row(
                    "SELECT count(*) FROM catalog_records WHERE instr(body,?1)>0",
                    [SECRET],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                leaks, 0,
                "synthetic credential must not enter derived records"
            );
            for needle in ["g1text0000item02", "g1text0031item02"] {
                let found = library
                    .handle
                    .list(
                        Query {
                            q: Some(needle.into()),
                            ..Default::default()
                        },
                        false,
                    )
                    .await
                    .unwrap();
                assert_eq!(found["records"].as_array().unwrap().len(), 1);
            }
        }
    }
}

fn source_hash(home: &Path) -> blake3::Hash {
    let mut files: Vec<_> = fs::read_dir(home.join("sessions"))
        .unwrap()
        .map(|v| v.unwrap().path())
        .collect();
    files.sort();
    let mut hash = blake3::Hasher::new();
    for file in files {
        hash.update(file.file_name().unwrap().as_encoded_bytes());
        hash.update(&fs::read(file).unwrap());
    }
    hash.finalize()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "G1 >200MiB pure English text with default cache pressure; run alone"]
async fn g1_pure_text_default_cache_pressure() {
    let temp = tempfile::tempdir().unwrap();
    let generated = Instant::now();
    let manifest = text_fixture(temp.path(), SEEDS[1].1, 1024);
    let generation_ms = generated.elapsed().as_secs_f64() * 1000.0;
    let home = temp.path().join("native");
    let data = temp.path().join("history");
    assert!(manifest["textBytes"].as_u64().unwrap() > 200 * 1024 * 1024);
    let source_before = source_hash(&home);
    let before = resources();
    let at = Instant::now();
    let library = start(&home, &data, LibraryConfig::default());
    assert!(published(&library, 900).await);
    let wall = at.elapsed().as_secs_f64();
    let cpu = workers(&library);
    let after = resources();
    let counts = cached_counts(&library);
    assert_eq!(counts.0, 1024);
    assert!(counts.1 < 1024 * 21);
    let page = library.handle.list(Query::default(), false).await.unwrap();
    assert!(
        page["coverage"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "cache_budget")
    );
    let db = read_db(&library.handle.shared).unwrap().unwrap();
    let mut statement = db.prepare("SELECT json_extract(metadata,'$.nativeThreadId') FROM catalog_entries WHERE source='default-native' AND generation IN(SELECT generation FROM catalog_sources)").unwrap();
    let ids: std::collections::HashSet<String> = statement
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let expected: std::collections::HashSet<_> = (1..=1024)
        .map(|i| uuid::Uuid::from_u128(i).to_string())
        .collect();
    assert_eq!(ids, expected);
    let (id,thread): (String,String) = db.query_row("SELECT e.id,json_extract(e.metadata,'$.nativeThreadId') FROM catalog_entries e WHERE e.source='default-native' AND e.generation IN(SELECT generation FROM catalog_sources) AND NOT EXISTS(SELECT 1 FROM catalog_records r WHERE r.generation=e.generation AND r.entry=e.id) LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let session = uuid::Uuid::parse_str(&thread).unwrap().as_u128() - 1;
    let window = library
        .handle
        .body(
            id,
            Query {
                window: true,
                limit: Some(8),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        window["records"]
            .to_string()
            .contains(&format!("g1text{session:04}item02"))
    );
    assert_eq!(source_hash(&home), source_before);
    emit(
        "pure_text_pressure",
        json!({"buildDebug":cfg!(debug_assertions),"manifest":manifest,"generationMs":generation_ms,"wallSeconds":wall,"workers":cpu,"resourcesBefore":before,"resourcesAfter":after,"entries":counts.0,"records":counts.1,"coverage":page["coverage"],"derivedBytes":disk(&data),"allIdsMatched":true,"uncachedSourceWindowReadable":true,"sourceBytesUnchanged":true,"scope":"default 512MiB configuration; admission intentionally omits body/search records; not complete-body-cache throughput"}),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn g1_fresh_cache_restores_omitted_text_but_budget_increase_alone_does_not() {
    let temp = tempfile::tempdir().unwrap();
    text_fixture(temp.path(), SEEDS[0].1, 32);
    let home = temp.path().join("native");
    let data = temp.path().join("history");
    let hash = source_hash(&home);
    let small = start(
        &home,
        &data,
        LibraryConfig {
            cache_limit_mi_b: 64,
            ..Default::default()
        },
    );
    assert!(published(&small, 60).await);
    let counts = cached_counts(&small);
    assert_eq!(counts.0, 32);
    assert!(counts.1 < 32 * 21);
    let (id, thread, generation): (String, String, String) = {
        let db = read_db(&small.handle.shared).unwrap().unwrap();
        db.query_row("SELECT e.id,json_extract(e.metadata,'$.nativeThreadId'),e.generation FROM catalog_entries e WHERE e.source='default-native' AND e.generation IN(SELECT generation FROM catalog_sources) AND NOT EXISTS(SELECT 1 FROM catalog_records r WHERE r.generation=e.generation AND r.entry=e.id) LIMIT 1",[], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap()
    };
    let session = uuid::Uuid::parse_str(&thread).unwrap().as_u128() - 1;
    // The first message also appears in the title; use a body-only marker so
    // metadata search cannot masquerade as complete body-cache coverage.
    let needle = format!("g1text{session:04}item02");
    let find = |needle: &str| Query {
        q: Some(needle.into()),
        ..Default::default()
    };
    assert!(
        small.handle.list(find(&needle), false).await.unwrap()["records"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    drop(small);
    let larger = start(&home, &data, LibraryConfig::default());
    let at = Instant::now();
    loop {
        let next = read_db(&larger.handle.shared)
            .ok()
            .flatten()
            .and_then(|db| {
                db.query_row(
                    "SELECT generation FROM catalog_sources WHERE id='default-native'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            });
        if next.as_ref().is_some_and(|v| v != &generation) {
            break;
        }
        assert!(at.elapsed() < Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(published(&larger, 60).await);
    assert_eq!(cached_counts(&larger), counts);
    let missing = larger.handle.list(find(&needle), false).await.unwrap();
    assert!(missing["records"].as_array().unwrap().is_empty());
    assert_eq!(missing["coverage"]["state"], "partial");
    let window = larger
        .handle
        .body(
            id,
            Query {
                window: true,
                limit: Some(8),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(window["records"].to_string().contains(&needle));
    drop(larger);
    // Use a fresh test-owned data root, without deleting any original source.
    let rebuilt = start(
        &home,
        &temp.path().join("rebuilt"),
        LibraryConfig::default(),
    );
    assert!(published(&rebuilt, 60).await);
    assert_eq!(cached_counts(&rebuilt), (32, 32 * 21));
    let found = rebuilt.handle.list(find(&needle), false).await.unwrap();
    assert_eq!(found["records"].as_array().unwrap().len(), 1);
    search_excerpt_only(&found);
    verify_all_text(&rebuilt, SEEDS[0].1);
    assert_eq!(source_hash(&home), hash);
    emit(
        "cache_rebuild_boundary",
        json!({"initialEntries":counts.0,"initialRecords":counts.1,"largerBudgetRestoresOmittedRecords":false,"sourceWindowReadable":true,"freshCacheRecords":32*21,"sourceBytesUnchanged":true}),
    );
}
