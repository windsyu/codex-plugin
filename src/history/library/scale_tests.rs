//! Opt-in synthetic G1 evidence. No user directories or private history are read.
use super::*;
use std::{
    collections::HashSet,
    fs,
    io::{BufWriter, Write},
    path::Path,
};

pub(super) fn emit(stage: &str, value: Value) {
    println!("R6_G1 {}", json!({"stage":stage,"data":value}));
}
pub(super) fn resources() -> Value {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) }, 0);
    let seconds = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 / 1e6;
    #[cfg(target_os = "macos")]
    let rss = usage.ru_maxrss as u64;
    #[cfg(not(target_os = "macos"))]
    let rss = usage.ru_maxrss as u64 * 1024;
    json!({"processCpuSeconds":seconds(usage.ru_utime)+seconds(usage.ru_stime),"processLifetimePeakRssBytes":rss,"scope":"test process including fixture generation; not separate worker CPU","queuePeak":null,"browserHeap":null,"browserDom":null,"paint":null})
}
pub(super) fn disk(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|v| v.ok())
        .filter(|v| v.file_type().is_file())
        .map(|v| v.metadata().unwrap().len())
        .sum()
}
fn corpus(home: &Path, projects: &Path, sessions: usize, large_body: bool) -> u64 {
    fs::create_dir_all(home.join("sessions")).unwrap();
    for project in 0..10 {
        fs::create_dir_all(projects.join(format!("p{project}"))).unwrap();
    }
    let padding = format!("data:image/png;base64,{}", "A".repeat(11 * 1024));
    let text = "ordinary synthetic readable sentence. ".repeat(if large_body { 450 } else { 1 });
    let mut total = 0;
    for session in 0..sessions {
        let path = home
            .join("sessions")
            .join(format!("rollout-{session:05}.jsonl"));
        let mut out = BufWriter::new(fs::File::create(&path).unwrap());
        writeln!(out,"{}",json!({"type":"session_meta","payload":{"id":uuid::Uuid::from_u128(session as u128+1),"cwd":projects.join(format!("p{}",session%10)),"timestamp":"2026-09-24T00:00:00Z"}})).unwrap();
        for item in 0..20 {
            // Opaque input image content is structurally omitted by the reader.
            // Actual JSON bytes are written; no sparse padding or malformed oversized lines.
            writeln!(out,"{}",json!({"type":"response_item","timestamp":"2026-09-24T00:00:00Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":format!("g1s{session:05}i{item:02} {text}")},{"type":"input_image","image_url":padding}]}})).unwrap();
        }
        out.flush().unwrap();
        total += fs::metadata(path).unwrap().len();
    }
    total
}
pub(super) fn generate_fixture(base: &Path, count: usize) -> (PathBuf, Value) {
    let home = base.join("native");
    let bytes = corpus(&home, &base.join("projects"), count, false);
    (
        home,
        json!({"sessions":count,"items":count*20,"sourceBytes":bytes,"kind":"native-jsonl-opaque-images-with-readable-text"}),
    )
}

pub(super) fn start(home: &Path, data: &Path, config: LibraryConfig) -> HistoryLibrary {
    HistoryLibrary::with_policy(
        home.into(),
        data.into(),
        Arc::new(move || Ok(config.clone())),
    )
    .unwrap()
}
pub(super) async fn published(library: &HistoryLibrary, deadline_seconds: u64) -> bool {
    let start = Instant::now();
    let mut report = Instant::now();
    loop {
        let states = library.handle.statuses();
        if !states.is_empty()
            && states.iter().all(|s| s.state != "indexing")
            && states.iter().any(|s| s.revision.is_some())
        {
            return true;
        }
        if report.elapsed() > Duration::from_secs(10) {
            emit(
                "progress",
                json!({"elapsedMs":start.elapsed().as_millis(),"sources":states,"resources":resources()}),
            );
            report = Instant::now();
        }
        if start.elapsed() > Duration::from_secs(deadline_seconds) {
            emit("publication_timeout", json!({"sources":states}));
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
async fn measured(handle: &LibraryHandle, query: Query, projects: bool, samples: usize) -> Value {
    let mut timings = vec![];
    let mut errors = vec![];
    for _ in 0..samples {
        let at = Instant::now();
        let result = handle.list(query.clone(), projects).await;
        timings.push(at.elapsed().as_secs_f64() * 1000.0);
        if let Err(error) = result {
            errors.push(error);
        }
    }
    timings.sort_by(f64::total_cmp);
    json!({"samples":samples,"p50Ms":timings[(samples as f64*0.50).ceil() as usize-1],"p95Ms":timings[(samples as f64*0.95).ceil() as usize-1],"p99Ms":timings[(samples as f64*0.99).ceil() as usize-1],"maxMs":timings[samples-1],"errors":errors})
}
async fn run(sessions: usize, full: bool) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("native");
    let data = temp.path().join("history");
    let projects = temp.path().join("projects");
    let before = resources();
    let generated = Instant::now();
    let (_, manifest) = generate_fixture(temp.path(), sessions);
    let bytes = manifest["sourceBytes"].as_u64().unwrap();
    emit(
        "manifest",
        json!({"sessions":sessions,"items":sessions*20,"sourceBytes":bytes,"generationMs":generated.elapsed().as_millis(),"fullAcceptanceCorpus":full,"resourcesBefore":before,"resourcesAfter":resources(),"buildDebug":cfg!(debug_assertions)}),
    );
    if full {
        assert!(bytes >= 2 * 1024 * 1024 * 1024);
    }
    let config = LibraryConfig::default();
    let at = Instant::now();
    let library = start(&home, &data, config.clone());
    let handle = library.handle();
    let cold = handle.list(Query::default(), false).await;
    let cold_ms = at.elapsed().as_secs_f64() * 1000.0;
    emit(
        "cold_api",
        json!({"elapsedMs":cold_ms,"result":cold.as_ref().map(|v|json!({"revision":v["revision"],"rows":v["records"].as_array().map(Vec::len),"coverage":v["coverage"]})),"scope":"API responsiveness only; not browser interactivity"}),
    );
    let ready = published(&library, 900).await;
    #[cfg(target_os = "macos")]
    emit(
        "publication_worker_cpu",
        super::resource_tests::workers(&library),
    );
    emit(
        "publication",
        json!({"ready":ready,"elapsedMs":at.elapsed().as_millis(),"sources":handle.statuses(),"derivedBytes":disk(&data),"resources":resources()}),
    );
    assert!(ready, "see publication_timeout evidence");
    let mut ids = HashSet::new();
    let mut cursor = None;
    let mut duplicate = false;
    let mut later = None;
    let coverage = loop {
        let value = handle
            .list(
                Query {
                    limit: Some(200),
                    cursor: cursor.clone(),
                    ..Query::default()
                },
                false,
            )
            .await
            .unwrap();
        for row in value["records"].as_array().unwrap() {
            if !ids.insert(row["entryId"].as_str().unwrap().to_string()) {
                duplicate = true;
            }
        }
        cursor = value["nextCursor"].as_str().map(String::from);
        if ids.len() >= sessions / 2 && later.is_none() {
            later = cursor.clone();
        }
        if cursor.is_none() {
            break value["coverage"].clone();
        }
    };
    let native_source = library
        .handle
        .shared
        .policy()
        .unwrap()
        .1
        .into_iter()
        .find(|s| s.id() == "default-native")
        .unwrap();
    let expected: HashSet<_> = (0..sessions)
        .map(|i| {
            Entry::new(
                &native_source,
                &uuid::Uuid::from_u128(i as u128 + 1).to_string(),
            )
            .entry_id
        })
        .collect();
    let missing = expected.difference(&ids).count();
    let unexpected = ids.difference(&expected).count();
    emit(
        "coverage",
        json!({"uniqueEntries":ids.len(),"expectedEntries":sessions,"missingEntries":missing,"unexpectedEntries":unexpected,"duplicates":duplicate,"coverage":coverage}),
    );
    if let Some(db) = read_db(&library.handle.shared).unwrap() {
        let counts: (u64,u64,u64) = db.query_row("SELECT (SELECT count(*) FROM catalog_entries WHERE generation IN (SELECT generation FROM catalog_sources)), (SELECT count(*) FROM catalog_records WHERE generation IN (SELECT generation FROM catalog_sources)), (SELECT coalesce(sum(length(CAST(body AS BLOB))),0) FROM catalog_records WHERE generation IN (SELECT generation FROM catalog_sources))", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        emit(
            "cached_counts",
            json!({"entries":counts.0,"records":counts.1,"bodyBytes":counts.2,"expectedSourceItems":sessions*20}),
        );
    }
    for (label, needle) in [
        ("first", "g1s00000i00".to_string()),
        ("last", format!("g1s{:05}i19", sessions - 1)),
    ] {
        let found = handle
            .list(
                Query {
                    q: Some(needle),
                    ..Query::default()
                },
                false,
            )
            .await;
        emit(
            "search_coverage",
            json!({"marker":label,"expectedSourceMatches":1,"indexedMatches":found.as_ref().ok().and_then(|v|v["records"].as_array()).map(Vec::len),"coverage":found.as_ref().ok().map(|v|&v["coverage"])}),
        );
    }
    let first = handle.list(Query::default(), false).await.unwrap();
    let project = project(projects.join("p0").to_str().unwrap()).unwrap().0;
    let cases = [
        ("entries", Query::default(), false),
        ("projects", Query::default(), true),
        (
            "filtered",
            Query {
                project_id: Some(project),
                ..Query::default()
            },
            false,
        ),
        (
            "later",
            Query {
                limit: Some(200),
                cursor: later,
                ..Query::default()
            },
            false,
        ),
        (
            "search_hit",
            Query {
                q: Some("g1s00000i00".into()),
                ..Query::default()
            },
            false,
        ),
        (
            "search_miss",
            Query {
                q: Some("g1-no-such-text".into()),
                ..Query::default()
            },
            false,
        ),
    ];
    let mut metadata_ok = true;
    #[cfg(target_os = "macos")]
    let query_cpu_before = super::resource_tests::workers(&library);
    for (name, q, p) in cases {
        let result = measured(&handle, q, p, 30).await;
        if !name.starts_with("search")
            && (result["p95Ms"].as_f64().unwrap() > 200.0
                || !result["errors"].as_array().unwrap().is_empty())
        {
            metadata_ok = false;
        }
        emit(name, result);
    }
    #[cfg(target_os = "macos")]
    emit(
        "warm_query_worker_cpu",
        json!({"before":query_cpu_before,"after":super::resource_tests::workers(&library),"scope":"six sequential query groups, 30 calls each; lifetime counters, subtract before from after"}),
    );
    // Explicit queued cancellation: cancellation is observed by the production worker,
    // and next-request recovery is measured separately from task abort delivery.
    let cancel_at = Instant::now();
    let mut tasks = vec![];
    for _ in 0..32 {
        let h = handle.clone();
        tasks.push(tokio::spawn(async move {
            h.list(
                Query {
                    q: Some("g1-no-such-text".into()),
                    ..Query::default()
                },
                false,
            )
            .await
        }));
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
    for task in &tasks {
        task.abort();
    }
    for task in tasks {
        let _ = task.await;
    }
    let abort_ms = cancel_at.elapsed().as_secs_f64() * 1000.0;
    let mut recovery_attempts = 0;
    let recovery = loop {
        recovery_attempts += 1;
        let result = handle.list(Query::default(), false).await;
        if result.is_ok() || cancel_at.elapsed() > Duration::from_secs(5) {
            break result;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    emit(
        "cancel",
        json!({"taskAbortMs":abort_ms,"nextQueryRecoveryMs":cancel_at.elapsed().as_secs_f64()*1000.0,"nextQueryOk":recovery.is_ok(),"recoveryAttempts":recovery_attempts,"activeSqlCancellation":"deadline only; abort completion is not proof worker stopped"}),
    );
    let rescan_at = Instant::now();
    let native_generation = || -> Option<String> {
        read_db(&library.handle.shared)
            .ok()
            .flatten()?
            .query_row(
                "SELECT generation FROM catalog_sources WHERE id='default-native'",
                [],
                |r| r.get(0),
            )
            .ok()
    };
    let old_generation = loop {
        if let Some(generation) = native_generation() {
            break generation;
        }
        if rescan_at.elapsed() > Duration::from_secs(5) {
            emit(
                "rescan_baseline_unavailable",
                json!({"sources":handle.statuses()}),
            );
            panic!("could not obtain the published generation before refresh");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let old_revision = first["revision"].clone();
    #[cfg(target_os = "macos")]
    let rescan_cpu_before = super::resource_tests::workers(&library);
    handle.refresh().await.unwrap();
    let changed_generation = loop {
        if native_generation()
            .as_ref()
            .is_some_and(|current| current != &old_generation)
        {
            break true;
        }
        if rescan_at.elapsed() >= Duration::from_secs(900) {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let rescan = changed_generation && published(&library, 900).await;
    #[cfg(target_os = "macos")]
    emit(
        "rescan_worker_cpu",
        json!({"before":rescan_cpu_before,"after":super::resource_tests::workers(&library),"scope":"unchanged refresh through committed generation; subtract lifetime counters"}),
    );
    let after = handle.list(Query::default(), false).await.unwrap();
    emit(
        "unchanged_rescan",
        json!({"ready":rescan,"elapsedMs":rescan_at.elapsed().as_millis(),"sameRevision":old_revision==after["revision"],"derivedBytes":disk(&data),"resources":resources()}),
    );
    drop(handle);
    drop(library);
    let restart_at = Instant::now();
    let warm = start(&home, &data, config);
    let warm_page = warm.handle().list(Query::default(), false).await;
    emit(
        "warm_restart",
        json!({"elapsedMs":restart_at.elapsed().as_millis(),"rows":warm_page.as_ref().ok().and_then(|v|v["records"].as_array()).map(Vec::len),"resources":resources(),"scope":"first published page only; full catalog not recounted after restart"}),
    );
    drop(warm);
    emit(
        "summary",
        json!({"metadataP95Passed":metadata_ok,"fullMetadataCountPassed":ids.len()==sessions&&!duplicate&&missing==0&&unexpected==0,"coldApiUnder1s":cold_ms<=1000.0,"browserGate":"not measured","queuePeak":"not instrumented","searchCoverage":coverage}),
    );
    assert!(
        ids.len() == sessions && !duplicate && missing == 0 && unexpected == 0,
        "all metadata must be published; see coverage output"
    );
    assert!(
        cold.is_ok(),
        "cold API must succeed, not merely return a fast error"
    );
    assert!(
        recovery.is_ok(),
        "query worker must recover after cancelled requests"
    );
    assert!(
        cold_ms <= 1000.0,
        "cold API exceeds 1 second (browser gate separate)"
    );
    assert!(
        metadata_ok,
        "warm metadata p95 exceeds 200ms or query errors; see case outputs"
    );
    assert!(
        rescan && old_revision == after["revision"],
        "unchanged rescan must publish a new generation with stable revision"
    );
    assert!(
        warm_page
            .as_ref()
            .is_ok_and(|v| v["records"].as_array().is_some_and(|v| !v.is_empty())),
        "warm restart must read the published index"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "G1 synthetic smoke: explicit local execution only"]
async fn g1_scale_smoke() {
    run(100, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "G1 writes >2GiB synthetic history and scans 10000 sessions; run alone"]
async fn g1_scale_10000_sessions_200000_items() {
    run(10_000, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "G1 default cache admission fault corpus; run alone"]
async fn g1_default_cache_pressure_is_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("native");
    let data = temp.path().join("history");
    let bytes = corpus(&home, &temp.path().join("projects"), 160, true);
    let library = start(&home, &data, LibraryConfig::default());
    let ready = published(&library, 900).await;
    let page = library.handle().list(Query::default(), false).await;
    emit(
        "default_cache_pressure",
        json!({"sourceBytes":bytes,"ready":ready,"sources":library.handle.statuses(),"derivedBytes":disk(&data),"pageCoverage":page.as_ref().ok().map(|v|&v["coverage"]),"resources":resources()}),
    );
    assert!(ready);
    assert!(
        page.unwrap()["coverage"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str() == Some("cache_budget"))
    );
}

/// Regression for metadata starvation by cached bodies; bounded, synthetic and fast.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn g1_body_budget_preserves_all_small_metadata_and_source_windows() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("native");
    let data = temp.path().join("history");
    let project = temp.path().join("project");
    fs::create_dir_all(home.join("sessions")).unwrap();
    fs::create_dir(&project).unwrap();
    for session in 0..160 {
        let mut file = BufWriter::new(
            fs::File::create(home.join("sessions").join(format!("s{session:03}.jsonl"))).unwrap(),
        );
        writeln!(file,"{}",json!({"type":"session_meta","payload":{"id":uuid::Uuid::from_u128(session+1),"cwd":project,"timestamp":"2026-09-24T00:00:00Z"}})).unwrap();
        for item in 0..8 {
            writeln!(file,"{}",json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":format!("正文 {session} {item} {}","合成历史规模".repeat(512))}]}})).unwrap();
        }
        file.flush().unwrap();
    }
    let config = LibraryConfig {
        cache_limit_mi_b: 64,
        ..Default::default()
    };
    let library = start(&home, &data, config.clone());
    assert!(published(&library, 60).await);
    let page = library
        .handle()
        .list(
            Query {
                limit: Some(200),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap();
    let initial_count = page["records"].as_array().unwrap().len();
    let partial = page["coverage"]["state"] == "partial";
    let (generation, uncached): (String, Option<String>) = {
        let db = read_db(&library.handle.shared).unwrap().unwrap();
        let generation = db
            .query_row(
                "SELECT generation FROM catalog_sources WHERE id='default-native'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let uncached=db.query_row("SELECT id FROM catalog_entries e WHERE generation IN(SELECT generation FROM catalog_sources) AND NOT EXISTS(SELECT 1 FROM catalog_records r WHERE r.generation=e.generation AND r.entry=e.id) LIMIT 1",[],|r|r.get(0)).optional().unwrap();
        (generation, uncached)
    };
    let window = if let Some(id) = uncached.as_ref() {
        library
            .handle()
            .body(
                id.clone(),
                Query {
                    window: true,
                    limit: Some(8),
                    ..Default::default()
                },
            )
            .await
            .ok()
    } else {
        None
    };
    let readable = window
        .as_ref()
        .is_some_and(|v| v["records"].as_array().is_some_and(|v| !v.is_empty()));
    emit(
        "metadata_reserve_initial",
        json!({"count":initial_count,"expected":160,"partial":partial,"uncachedEntryExists":uncached.is_some(),"uncachedSourceWindowReadable":readable,"derivedBytes":disk(&data)}),
    );
    library.handle().refresh().await.unwrap();
    let at = Instant::now();
    loop {
        let next = read_db(&library.handle.shared)
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
        if next.as_ref().is_some_and(|next| next != &generation) {
            break;
        }
        assert!(at.elapsed() < Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(published(&library, 60).await);
    let refreshed = library
        .handle()
        .list(
            Query {
                limit: Some(200),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap()["records"]
        .as_array()
        .unwrap()
        .len();
    drop(library);
    let warm = start(&home, &data, config);
    let restarted = warm
        .handle()
        .list(
            Query {
                limit: Some(200),
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap()["records"]
        .as_array()
        .unwrap()
        .len();
    emit(
        "metadata_reserve_summary",
        json!({"initial":initial_count,"refreshed":refreshed,"restarted":restarted,"expected":160,"derivedBytes":disk(&data),"resources":resources()}),
    );
    assert_eq!(
        initial_count, 160,
        "bodies must not starve small metadata; see aggregated diagnostics"
    );
    assert_eq!(refreshed, 160);
    assert_eq!(restarted, 160);
    assert!(partial && readable);
}
