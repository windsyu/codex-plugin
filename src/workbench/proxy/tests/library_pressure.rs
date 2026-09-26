//! Real derived-catalog pressure must remain separate from native I/O workers.
use super::*;
use crate::history::library::{HistoryLibrary, Query};
use crate::workbench::config::{ConfigService, Overrides, Prepared};
use crate::workbench::decode::DecoderLimits;
use crate::workbench::live::{LiveHub, LiveLimits};
use crate::workbench::observe::Observer;
use crate::workbench::recording::{Recorder, RecorderOptions};
use crate::workbench::redaction::RedactionPolicy;
use crate::workbench::terminal::TerminalHost;
use std::io::Write;
use std::path::Path;

fn synthetic_rollout(home: &Path, cwd: &Path, index: usize) {
    let mut file = std::fs::File::create(home.join(format!("sessions/{index}.jsonl"))).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({"type":"session_meta","payload":{
            "id":uuid::Uuid::new_v4(),"cwd":cwd,"timestamp":"2026-09-24T00:00:00Z"
        }})
    )
    .unwrap();
    for line in 0..64 {
        writeln!(file, "{}", serde_json::json!({"type":"response_item","payload":{
            "type":"message","role":"user","content":[{"text":format!("Synthetic history {index}/{line} {}", "x".repeat(256))}]
        }})).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_library_pressure_keeps_sse_pty_and_durable_recording_live() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("user");
    let native_home = user_home.join(".codex");
    let cwd = temp.path().join("synthetic-project");
    std::fs::create_dir_all(native_home.join("sessions")).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    synthetic_rollout(&native_home, &cwd, 0);
    let prepared = Prepared::load(
        &native_home,
        &cwd,
        &crate::workbench::paths::WorkbenchPaths::from_user_home(&user_home).unwrap(),
        None,
        Overrides::default(),
    )
    .unwrap();
    let data = prepared.data_dir.clone();
    let config = ConfigService::start(prepared).unwrap();
    let library =
        HistoryLibrary::start(native_home.clone(), data.clone(), config.handle()).unwrap();
    let handle = library.handle();
    timeout(Duration::from_secs(8), async {
        loop {
            if handle.statuses().iter().any(|s| {
                s.id == "default-native" && s.indexed_entries == 1 && s.state != "indexing"
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let hub = LiveHub::new(LiveLimits::default());
    let mut options = RecorderOptions::new(data.clone(), &cwd, "Synthetic library pressure".into());
    options.sync_interval = Duration::from_millis(50);
    let recorder = Recorder::start(&hub, options).unwrap();
    let (capture, receiver) = capture::channel(1024 * 1024, 64);
    let _observer = Observer::start(
        receiver,
        hub.clone(),
        RedactionPolicy::new(vec![]).unwrap(),
        DecoderLimits::default(),
    )
    .unwrap();
    let mut command = portable_pty::CommandBuilder::new("/bin/cat");
    command.cwd(&cwd);
    command.env("HOME", &user_home);
    command.env("USERPROFILE", &user_home);
    command.env("CODEX_HOME", &native_home);
    let host = TerminalHost::spawn(hub.epoch(), command, 24, 80).unwrap();
    let terminal = host.handle();
    let attached = terminal.attach().await.unwrap();
    let grant = terminal.claim(attached.connection_id).await.unwrap();

    for index in 1..128 {
        synthetic_rollout(&native_home, &cwd, index);
    }
    handle.refresh().await.unwrap();
    timeout(DEADLINE, async {
        while !handle
            .statuses()
            .iter()
            .any(|s| s.id == "default-native" && s.state == "indexing")
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let mut measurements = Vec::new();
    // First exercise the actual parser, then keep an external exclusive SQLite
    // transaction open during a bounded query flood and the same native I/O.
    let mut lock = None;
    let mut flood = None;
    let mut flood_started = None;
    for phase in 0..2 {
        if phase == 1 {
            let db = rusqlite::Connection::open(data.join("library/catalog.sqlite")).unwrap();
            db.busy_timeout(Duration::from_secs(3)).unwrap();
            db.execute_batch("BEGIN EXCLUSIVE").unwrap();
            lock = Some(db);
            let h = handle.clone();
            flood_started = Some(Instant::now());
            flood = Some(tokio::spawn(async move {
                futures_util::future::join_all((0..64).map(|_| h.list(Query::default(), false)))
                    .await
            }));
            tokio::task::yield_now().await;
        }
        let payload = format!(
            "data: {}\n\n",
            serde_json::json!({"type":"response.output_text.done","item_id":format!("pressure_{phase}"),"content_index":0,"text":format!("R6_G2_LIVE_{phase}")})
        );
        let expected = payload.clone();
        let (tail_tx, tail_rx) = oneshot::channel();
        let gate = Arc::new(Mutex::new(Some(tail_rx)));
        let upstream = fixture(move |_| {
            let tail = gate.lock().unwrap().take().unwrap();
            let payload = payload.clone();
            async move {
                Response::builder()
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(async_stream::stream! {
                        yield Ok::<_, std::io::Error>(Bytes::from(payload));
                        tail.await.unwrap();
                        yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
                    }))
                    .unwrap()
            }
        })
        .await;
        let proxy = proxy_for(upstream.address, capture.clone()).await;
        let start = Instant::now();
        let mut response = client()
            .get(format!("{}/responses", proxy.child_base_url()))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.chunk().await.unwrap().unwrap().as_ref(),
            expected.as_bytes()
        );
        let first_ms = start.elapsed().as_secs_f64() * 1000.0;
        let pty_start = Instant::now();
        let marker = format!("R6_G2_PTY_{phase}");
        terminal
            .input(
                attached.connection_id,
                grant.generation,
                phase + 1,
                format!("{marker}\n").into_bytes(),
            )
            .await
            .unwrap();
        timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = terminal.attach().await.unwrap().snapshot;
                let mut parser = vt100::Parser::new(snapshot.rows, snapshot.cols, 0);
                parser.process(&snapshot.screen);
                parser.process(&snapshot.replay);
                if parser.screen().contents().contains(&marker) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let pty_ms = pty_start.elapsed().as_secs_f64() * 1000.0;
        timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = hub.snapshot();
                let saved = hub.recorder_status();
                assert!(saved.persisted_through_view_seq <= saved.saved_through_view_seq);
                assert!(saved.saved_through_view_seq <= snapshot.view_seq);
                if snapshot.model_items().len() > phase as usize
                    && saved.persisted_through_view_seq == snapshot.view_seq
                    && snapshot.view_seq > 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let durable_ms = start.elapsed().as_secs_f64() * 1000.0;
        tail_tx.send(()).unwrap();
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            b"data: [DONE]\n\n"
        );
        measurements.push(serde_json::json!({"phase":phase,"httpFirstChunkMs":first_ms,"ptyEchoMs":pty_ms,"durableProgressFromRequestMs":durable_ms,"httpCompleteIncludingTestGateMs":start.elapsed().as_secs_f64()*1000.0}));
    }
    let results = timeout(Duration::from_secs(4), flood.unwrap())
        .await
        .unwrap()
        .unwrap();
    let query_total_ms = flood_started.unwrap().elapsed().as_secs_f64() * 1000.0;
    assert!(query_total_ms < 4000.0);
    let busy = results
        .iter()
        .filter(|r| matches!(r, Err("library_busy")))
        .count();
    let locked = results
        .iter()
        .filter(|r| matches!(r, Err("cache_invalid")))
        .count();
    assert!(
        busy >= 47,
        "16 queued plus one active request bound: {results:?}"
    );
    assert!(
        locked > 0,
        "actual derived-cache read contention was exercised"
    );
    assert!(
        results
            .iter()
            .all(|r| matches!(r, Err("library_busy" | "cache_invalid" | "library_timeout")))
    );
    assert_eq!(capture.stats().dropped_chunks, 0);
    let drop_start = Instant::now();
    // Keep the external lock held: cancellation must not depend on its release.
    timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || drop(library)),
    )
    .await
    .unwrap()
    .unwrap();
    let drop_ms = drop_start.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(
        handle.list(Query::default(), false).await,
        Err("library_busy")
    );
    lock.unwrap().execute_batch("ROLLBACK").unwrap();
    terminal.stop().await.unwrap();
    drop(recorder);
    println!(
        "{}",
        serde_json::json!({"check":"r6-g2-library-isolation","syntheticSessions":128,"queries":64,"busy":busy,"sqliteLocked":locked,"queryFloodTotalMs":query_total_ms,"libraryDropMs":drop_ms,"measurements":measurements,"observationDrops":0})
    );
}
