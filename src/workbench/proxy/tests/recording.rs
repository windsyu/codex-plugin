use super::*;
use crate::workbench::decode::DecoderLimits;
use crate::workbench::live::{LiveHub, LiveLimits};
use crate::workbench::observe::Observer;
use crate::workbench::recording::{Recorder, RecorderOptions};
use crate::workbench::redaction::RedactionPolicy;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_history_management_keeps_model_stream_pty_and_recording_live() {
    use crate::workbench::config::{ConfigService, Overrides, Prepared};
    use crate::workbench::recording::management::{Query, Service};
    use crate::workbench::terminal::TerminalHost;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let cwd = dir.path().join("project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    let prepared = Prepared::load(
        &home,
        &cwd,
        &crate::workbench::paths::WorkbenchPaths::from_user_home(home.parent().unwrap()).unwrap(),
        None,
        Overrides::default(),
    )
    .unwrap();
    let root = prepared.data_dir.clone();
    let config = ConfigService::start(prepared).unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let recorder = Recorder::start(
        &hub,
        RecorderOptions::new(root.clone(), &cwd, "Synthetic manager pause".into()),
    )
    .unwrap();
    let manager = Service::start(
        root,
        blake3::hash(cwd.as_os_str().as_encoded_bytes())
            .to_hex()
            .to_string(),
        hub.epoch(),
        config.handle(),
    )
    .unwrap();
    let handle = manager.handle();
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let holding = tokio::spawn(async move {
        handle
            .query(Query::HoldWorker {
                entered: entered_tx,
                release: wait,
            })
            .await
    });
    timeout(DEADLINE, entered_rx).await.unwrap().unwrap();
    let payload = "data: {\"type\":\"response.output_text.done\",\"item_id\":\"manager_message\",\"content_index\":0,\"text\":\"R31_MANAGEMENT_LIVE\"}\n\n";
    let (tail_tx, tail_rx) = oneshot::channel();
    let tail = Arc::new(Mutex::new(Some(tail_rx)));
    let upstream = fixture(move |_| {
        let tail = tail.lock().unwrap().take().unwrap();
        async move {
            Response::builder()
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(async_stream::stream! {
                    yield Ok::<_, std::io::Error>(Bytes::from_static(payload.as_bytes()));
                    tail.await.unwrap();
                    yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
                }))
                .unwrap()
        }
    })
    .await;
    let (capture, receiver) = capture::channel(1024 * 1024, 64);
    let proxy = proxy_for(upstream.address, capture.clone()).await;
    let _observer = Observer::start(
        receiver,
        hub.clone(),
        RedactionPolicy::new(vec![]).unwrap(),
        DecoderLimits::default(),
    )
    .unwrap();
    let mut command = portable_pty::CommandBuilder::new("/bin/cat");
    command.cwd(&cwd);
    command.env("CODEX_HOME", &home);
    let host = TerminalHost::spawn(hub.epoch(), command, 24, 80).unwrap();
    let terminal = host.handle();
    let attached = terminal.attach().await.unwrap();
    let grant = terminal.claim(attached.connection_id).await.unwrap();
    let started = Instant::now();
    let mut response = client()
        .get(format!("{}/responses", proxy.child_base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.chunk().await.unwrap().unwrap().as_ref(),
        payload.as_bytes()
    );
    terminal
        .input(
            attached.connection_id,
            grant.generation,
            1,
            b"R31_PTY_LIVE\n".to_vec(),
        )
        .await
        .unwrap();
    timeout(Duration::from_secs(2), async {
        loop {
            let a = terminal.attach().await.unwrap();
            let mut parser = vt100::Parser::new(a.snapshot.rows, a.snapshot.cols, 0);
            parser.process(&a.snapshot.screen);
            parser.process(&a.snapshot.replay);
            if parser.screen().contents().contains("R31_PTY_LIVE")
                && !hub.snapshot().model_items().is_empty()
                && hub.recorder_status().saved_through_view_seq == hub.snapshot().view_seq
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !holding.is_finished(),
        "management remains blocked while native I/O and saved content advance"
    );
    let progress_ms = started.elapsed().as_millis();
    tail_tx.send(()).unwrap();
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        b"data: [DONE]\n\n"
    );
    // Concurrent queue pressure has a bounded rejection path instead of leaking
    // unbounded requests or borrowing the network/PTY worker for disk access.
    let waiting = (0..16)
        .map(|_| {
            let h = manager.handle();
            tokio::spawn(async move { h.query(Query::Usage { cursor: None }).await })
        })
        .collect::<Vec<_>>();
    let mut busy = 0;
    for request in waiting {
        if matches!(
            request.await.unwrap(),
            Err(crate::workbench::recording::management::Error::Busy)
        ) {
            busy += 1;
        }
    }
    assert!(busy >= 8);
    assert!(holding.await.unwrap().is_err());
    release.send(()).unwrap();
    assert_eq!(capture.stats().dropped_chunks, 0);
    println!(
        "{}",
        serde_json::json!({"check":"r31-management-isolation","managerBlockedAtLeastMs":3000,"streamAndPtyAndSaveProgressMs":progress_ms,"boundedBusyQueries":busy,"observationDrops":0})
    );
    drop(recorder);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paused_full_recorder_and_slow_page_do_not_delay_model_forwarding() {
    let payload = "data: {\"type\":\"response.created\",\"response\":{\"id\":\"recording_response\"}}\n\ndata: {\"type\":\"response.output_text.done\",\"item_id\":\"recording_message\",\"content_index\":0,\"text\":\"R3_FORWARD_LIVE\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"recording_response\"}}\n\n";
    let upstream = fixture(move |_| async move {
        Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(payload))
            .unwrap()
    })
    .await;
    let (capture, receiver) = capture::channel(8 * 1024 * 1024, 256);
    let proxy = proxy_for(upstream.address, capture.clone()).await;
    let hub = LiveHub::new(LiveLimits {
        client_events: 4,
        ..LiveLimits::default()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut options = RecorderOptions::new(
        dir.path().join("history"),
        dir.path(),
        "Synthetic stalled recorder".into(),
    );
    options.queue_events = 1;
    options.queue_bytes = 1024;
    let faults = options.faults.clone();
    faults.pause_ms.store(5000, Ordering::Release);
    let recorder = Recorder::start(&hub, options).unwrap();
    timeout(Duration::from_secs(1), async {
        while !faults.pause_active.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let _observer = Observer::start(
        receiver,
        hub.clone(),
        RedactionPolicy::new(vec![]).unwrap(),
        DecoderLimits::default(),
    )
    .unwrap();
    let mut slow = hub.subscribe(hub.epoch(), 0).unwrap();
    let client = client();
    let start = Instant::now();
    let mut milliseconds = Vec::new();
    for _ in 0..20 {
        let started = Instant::now();
        let bytes = client
            .get(format!("{}/responses", proxy.child_base_url()))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        milliseconds.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(bytes.as_ref(), payload.as_bytes());
    }
    assert!(
        start.elapsed() < Duration::from_millis(1500),
        "forwarding waited for the 5-second recording stall"
    );
    timeout(Duration::from_secs(2), async {
        while hub.snapshot().model_items().len() < 20 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(capture.stats().dropped_chunks, 0);
    assert!(slow.recv().await.is_none());
    assert_eq!(hub.recorder_status().persisted_through_view_seq, 0);
    faults.pause_ms.store(0, Ordering::Release);
    timeout(Duration::from_secs(6), async {
        while hub.recorder_status().saved_through_view_seq != hub.snapshot().view_seq {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(recorder);
    milliseconds.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::json!({"check":"r3-recording-isolation","requests":20,"recorderPauseMs":5000,"clientRoundtripP50Ms":milliseconds[9],"clientRoundtripP95Ms":milliseconds[18],"clientRoundtripP99Ms":milliseconds[19],"observationDrops":0,"slowReaderClosed":true})
    );
}
