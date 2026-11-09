use super::history::{HistoryError, Query};
use super::*;
use crate::workbench::decode::{Change, Decoded, TextKey};
use crate::workbench::live::LiveLimits;
use crate::workbench::redaction::RedactionPolicy;

fn publish(hub: &LiveHub, key: &TextKey, text: &str) {
    hub.apply(Decoded {
        request_id: key.request_id,
        capture_seq: 1,
        received_at: Instant::now(),
        change: Change::TextDelta {
            key: key.clone(),
            text: RedactionPolicy::new(vec!["synthetic-private-key".into()])
                .unwrap()
                .scrub(text),
        },
    });
}
fn key() -> TextKey {
    TextKey {
        request_id: Uuid::new_v4(),
        response_id: Some("resp_history".into()),
        wire_item_id: "msg_history".into(),
        content_index: 0,
    }
}
fn options(dir: &std::path::Path) -> RecorderOptions {
    let mut options = RecorderOptions::new(dir.join("history"), dir, "synthetic-project".into());
    options.sync_interval = Duration::from_millis(30);
    options
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let start = Instant::now();
    while !predicate() {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[tokio::test]
async fn native_direct_capture_status_survives_journal_replay_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    hub.model_capture_unavailable();
    let recorder = Recorder::start(&hub, options(dir.path())).unwrap();
    let history = recorder.history.clone();
    hub.enable_user_reader();
    hub.user_issue(
        Uuid::new_v4(),
        0,
        crate::workbench::rollout::UserIssue::ReadFailed,
    );
    wait(|| hub.recorder_status().saved_through_view_seq == hub.snapshot().view_seq);
    assert_eq!(hub.snapshot().capture, "unavailable");
    drop(recorder);
    let read = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    assert_eq!(read["snapshot"]["capture"], "unavailable");
    assert_eq!(read["run"]["historyCoverage"], "partial");
    assert!(
        read["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap["reason"] == "model_capture_unavailable")
    );
    assert!(
        read["snapshot"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["text"]
                .as_str()
                .is_some_and(|text| text.contains("模型实时捕获不可用")))
    );
}

#[tokio::test]
async fn records_safe_content_and_replays_after_clean_exit_without_native_input() {
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let options = options(dir.path());
    let root = options.root.clone();
    let recorder = Recorder::start(&hub, options).unwrap();
    let history = recorder.history.clone();
    let key = key();
    publish(&hub, &key, "hello synthetic-private-key");
    publish(&hub, &key, " world");
    wait(|| hub.recorder_status().saved_through_view_seq == hub.snapshot().view_seq);
    let seq = hub.snapshot().view_seq;
    assert!(seq > 0);
    drop(recorder);
    let read = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    assert_eq!(read["run"]["state"], "ended");
    assert_eq!(read["snapshot"]["viewSeq"], seq);
    assert_eq!(
        read["snapshot"]["items"],
        serde_json::json!(hub.snapshot().items)
    );
    assert!(!read.to_string().contains("synthetic-private-key"));
    assert_eq!(read["uncertainTail"], false);
    let list = history.query(Query::List { cursor: None }).await.unwrap();
    assert_eq!(list["runs"].as_array().unwrap().len(), 1);
    std::fs::write(root.join("index.sqlite"), b"corrupt cache").unwrap();
    assert_eq!(
        history.query(Query::List { cursor: None }).await.unwrap()["indexState"],
        "ready"
    );
    assert_eq!(
        history
            .query(Query::List {
                cursor: Some("bad".into())
            })
            .await
            .unwrap_err(),
        HistoryError::InvalidCursor
    );
}

#[test]
fn no_saved_watermark_before_sync_and_status_never_creates_view_events() {
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let mut options = options(dir.path());
    options.sync_interval = Duration::from_secs(30);
    let recorder = Recorder::start(&hub, options).unwrap();
    wait(|| hub.recorder_status().state == "saved");
    publish(&hub, &key(), "pending");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(hub.recorder_status().persisted_through_view_seq, 0);
    assert_eq!(hub.recorder_status().state, "pending");
    let seq = hub.snapshot().view_seq;
    drop(recorder);
    assert_eq!(hub.recorder_status().persisted_through_view_seq, seq);
    assert_eq!(hub.snapshot().view_seq, seq);
}

#[tokio::test]
async fn disk_full_freezes_prefix_and_recovers_in_new_segment_with_explicit_gap() {
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let options = options(dir.path());
    let faults = options.faults.clone();
    let recorder = Recorder::start(&hub, options).unwrap();
    let history = recorder.history.clone();
    let key = key();
    publish(&hub, &key, "saved");
    wait(|| hub.recorder_status().persisted_through_view_seq > 0);
    let prefix = hub.recorder_status().persisted_through_view_seq;
    faults.write_error.store(true, Ordering::Release);
    publish(&hub, &key, " unsaved");
    wait(|| hub.recorder_status().state == "degraded");
    assert_eq!(hub.recorder_status().persisted_through_view_seq, prefix);
    let start = Instant::now();
    for _ in 0..100 {
        publish(&hub, &key, ".");
    }
    assert!(start.elapsed() < Duration::from_secs(1));
    faults.write_error.store(false, Ordering::Release);
    wait(|| hub.recorder_status().saved_through_view_seq == hub.snapshot().view_seq);
    assert_eq!(hub.recorder_status().persisted_through_view_seq, prefix);
    assert_eq!(hub.recorder_status().history_coverage, "partial");
    drop(recorder);
    let read = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    assert!(!read["gaps"].as_array().unwrap().is_empty());
    assert_eq!(
        read["snapshot"]["items"],
        serde_json::json!(hub.snapshot().items)
    );
}

#[test]
fn private_storage_rejects_symlinks_shared_permissions_and_invalid_blobs() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let root = Directory::root(&dir.path().join("private")).unwrap();
    std::fs::write(dir.path().join("outside"), b"keep").unwrap();
    symlink(dir.path().join("outside"), root.path.join("trap")).unwrap();
    assert!(root.open("trap", false).is_err());
    assert!(root.dir("../escape", true).is_err());
    assert!(root.read_blob_limited("../../outside", 1024).is_err());
    let blob = root.blob(b"bounded blob").unwrap();
    assert!(root.read_blob_limited(&blob, 3).is_err());
    assert_eq!(root.read_blob_limited(&blob, 12).unwrap(), b"bounded blob");
    std::fs::set_permissions(&root.path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Directory::root(&root.path).is_err());
    assert_eq!(std::fs::read(dir.path().join("outside")).unwrap(), b"keep");
}

#[test]
fn default_parent_is_created_privately_and_failed_creation_keeps_live_reading() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    for blocked in [false, true] {
        let parent = dir.path().join(if blocked {
            "blocked-root"
        } else {
            ".codex-web"
        });
        if blocked {
            std::fs::write(&parent, b"existing file").unwrap();
        }
        let hub = LiveHub::new(LiveLimits::default());
        let mut opts = options(&parent);
        opts.private_parent = Some(parent.clone());
        let recorder = Recorder::start(&hub, opts).unwrap();
        publish(&hub, &key(), "still live");
        if blocked {
            wait(|| hub.recorder_status().error == Some("storage_failed"));
            assert_eq!(hub.recorder_status().persisted_through_view_seq, 0);
            assert_eq!(hub.snapshot().model_items()[0].text, "still live");
            assert_eq!(std::fs::read(&parent).unwrap(), b"existing file");
        } else {
            wait(|| hub.recorder_status().saved_through_view_seq == hub.snapshot().view_seq);
            for path in [&parent, &parent.join("history")] {
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        }
        drop(recorder);
    }
}

#[tokio::test]
async fn paused_recorder_and_full_queue_do_not_block_live_content_and_restart_from_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let mut options = options(dir.path());
    options.queue_events = 1;
    options.queue_bytes = 1024;
    let faults = options.faults.clone();
    faults.pause_ms.store(500, Ordering::Release);
    let recorder = Recorder::start(&hub, options).unwrap();
    let history = recorder.history.clone();
    let key = key();
    let start = Instant::now();
    publish(&hub, &key, &"x".repeat(2048));
    publish(&hub, &key, " live");
    assert!(start.elapsed() < Duration::from_millis(100));
    assert_eq!(hub.snapshot().model_items()[0].text.len(), 2053);
    assert_eq!(hub.recorder_status().state, "degraded");
    assert_eq!(hub.recorder_status().persisted_through_view_seq, 0);
    faults.pause_ms.store(0, Ordering::Release);
    wait(|| hub.recorder_status().saved_through_view_seq == hub.snapshot().view_seq);
    drop(recorder);
    let restored = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    assert_eq!(restored["snapshot"]["items"], json!(hub.snapshot().items));
    assert!(!restored["gaps"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn fsync_failure_never_promotes_complete_but_unsaved_journal_lines() {
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let options = options(dir.path());
    let faults = options.faults.clone();
    let recorder = Recorder::start(&hub, options).unwrap();
    let history = recorder.history.clone();
    wait(|| hub.recorder_status().state == "saved");
    faults.sync_error.store(true, Ordering::Release);
    publish(&hub, &key(), "visible but not durable");
    wait(|| hub.recorder_status().state == "degraded");
    assert_eq!(hub.recorder_status().persisted_through_view_seq, 0);
    drop(recorder);
    let restored = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    assert_eq!(restored["run"]["state"], "unclean");
    assert!(restored["snapshot"]["items"].as_array().unwrap().is_empty());
    assert!(
        restored["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["code"] == "unsaved_tail")
    );
    assert_eq!(restored["uncertainTail"], true);
}

#[tokio::test]
async fn partial_and_corrupt_committed_lines_are_diagnosed_without_using_stale_snapshot_or_index() {
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let options = options(dir.path());
    let root = options.root.clone();
    let recorder = Recorder::start(&hub, options).unwrap();
    let history = recorder.history.clone();
    let key = key();
    publish(&hub, &key, "first");
    publish(&hub, &key, " second");
    drop(recorder);
    let run = root.join("runs").join(hub.epoch().to_string());
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run.join("meta.json")).unwrap()).unwrap();
    let journal = run.join(format!(
        "observations.{}.jsonl",
        meta["segments"][0]["id"].as_str().unwrap()
    ));
    let original = std::fs::read(&journal).unwrap();
    // The optional derived snapshot is never used to hide journal damage.
    std::fs::write(
        run.join("snapshot.json"),
        b"unknown derived snapshot format",
    )
    .unwrap();
    std::fs::write(&journal, &original[..original.len() - 5]).unwrap();
    let partial = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    assert!(
        partial["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["code"] == "partial_line")
    );
    assert_eq!(partial["snapshot"]["viewSeq"], 1);
    let mut corrupt = original.clone();
    let position = corrupt.iter().position(|b| *b == b'\n').unwrap() + 2;
    corrupt[position] = b'!';
    std::fs::write(&journal, corrupt).unwrap();
    let broken = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    assert_eq!(broken["snapshot"]["viewSeq"], 1);
    assert!(
        broken["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["code"] == "invalid_record")
    );
    std::fs::write(&journal, &original).unwrap();
    assert_eq!(
        history
            .query(Query::Run { epoch: hub.epoch() })
            .await
            .unwrap()["snapshot"]["viewSeq"],
        2
    );
}

#[tokio::test]
async fn missing_committed_line_freezes_verified_prefix_across_later_recovery_segments() {
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let options = options(dir.path());
    let root = options.root.clone();
    let faults = options.faults.clone();
    let recorder = Recorder::start(&hub, options).unwrap();
    let history = recorder.history.clone();
    let key = key();
    publish(&hub, &key, "first");
    publish(&hub, &key, " second");
    wait(|| hub.recorder_status().persisted_through_view_seq == 2);
    faults.write_error.store(true, Ordering::Release);
    publish(&hub, &key, " recovered checkpoint");
    wait(|| hub.recorder_status().state == "degraded");
    faults.write_error.store(false, Ordering::Release);
    wait(|| hub.recorder_status().saved_through_view_seq == 3);
    drop(recorder);
    let run = root.join("runs").join(hub.epoch().to_string());
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run.join("meta.json")).unwrap()).unwrap();
    assert!(meta["segments"].as_array().unwrap().len() >= 2);
    let journal = run.join(format!(
        "observations.{}.jsonl",
        meta["segments"][0]["id"].as_str().unwrap()
    ));
    let bytes = std::fs::read(&journal).unwrap();
    let first_line = bytes.iter().position(|b| *b == b'\n').unwrap() + 1;
    std::fs::write(journal, &bytes[..first_line]).unwrap();
    let restored = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    assert_eq!(restored["snapshot"]["viewSeq"], 3);
    assert!(
        restored["snapshot"]
            .to_string()
            .contains("recovered checkpoint")
    );
    assert_eq!(restored["snapshot"]["persistedThroughViewSeq"], 1);
    assert!(
        restored["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "checkpoint_mismatch")
    );
    let base = run
        .join("blobs")
        .join(meta["segments"][0]["base"].as_str().unwrap());
    std::fs::write(base, b"corrupt independent segment base").unwrap();
    let restored = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    assert_eq!(restored["snapshot"]["viewSeq"], 3);
    assert_eq!(restored["snapshot"]["persistedThroughViewSeq"], 0);
    assert!(
        restored["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "snapshot_invalid")
    );
    let mut unsupported = meta;
    unsupported["formatVersion"] = json!(999);
    std::fs::write(
        run.join("meta.json"),
        serde_json::to_vec(&unsupported).unwrap(),
    )
    .unwrap();
    assert_eq!(
        history
            .query(Query::Run { epoch: hub.epoch() })
            .await
            .unwrap_err(),
        HistoryError::Unavailable
    );
}

#[tokio::test]
async fn reading_eviction_is_a_saved_checkpoint_not_a_false_recording_failure() {
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits {
        items: 2,
        ..LiveLimits::default()
    });
    let options = options(dir.path());
    let recorder = Recorder::start(&hub, options).unwrap();
    let history = recorder.history.clone();
    for i in 0..8 {
        publish(&hub, &key(), &format!("model {i}"));
    }
    drop(recorder);
    let read = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    assert_eq!(read["snapshot"]["items"], json!(hub.snapshot().items));
    assert!(read["gaps"].as_array().unwrap().is_empty());
    assert_eq!(
        read["snapshot"]["persistedThroughViewSeq"],
        hub.snapshot().view_seq
    );
    let early = history
        .query(Query::Earlier {
            epoch: hub.epoch(),
            before: 2,
        })
        .await
        .unwrap();
    assert_eq!(early["snapshot"]["viewSeq"], 2);
    assert!(early["snapshot"].to_string().contains("model 0"));
    assert!(!early["snapshot"].to_string().contains("model 7"));
}

#[test]
fn crash_worker_fixture() {
    let Some(path) = std::env::var_os("WORKBENCH_R3_CRASH_FIXTURE") else {
        return;
    };
    let dir = std::path::PathBuf::from(path);
    let hub = LiveHub::new(LiveLimits::default());
    let mut options = options(&dir);
    options.sync_interval = Duration::from_secs(1);
    let _recorder = Recorder::start(&hub, options).unwrap();
    let key = key();
    publish(&hub, &key, "durable");
    wait(|| hub.recorder_status().saved_through_view_seq > 0);
    publish(&hub, &key, " lost tail");
    std::thread::sleep(Duration::from_millis(150));
    std::fs::write(dir.join("ready.tmp"), hub.epoch().to_string()).unwrap();
    std::fs::rename(dir.join("ready.tmp"), dir.join("ready")).unwrap();
    loop {
        std::thread::park();
    }
}

#[tokio::test]
async fn killed_service_restores_only_committed_prefix_and_marks_uncertain_tail() {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut child = Child(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "workbench::recording::tests::crash_worker_fixture",
                "--nocapture",
            ])
            .env("WORKBENCH_R3_CRASH_FIXTURE", dir.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait(|| dir.path().join("ready").exists());
    let epoch = std::fs::read_to_string(dir.path().join("ready"))
        .unwrap()
        .parse()
        .unwrap();
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let fresh = LiveHub::new(LiveLimits::default());
    let options = options(dir.path());
    let recorder = Recorder::start(&fresh, options).unwrap();
    let read = recorder.history.query(Query::Run { epoch }).await.unwrap();
    assert_eq!(read["run"]["state"], "unclean");
    assert_eq!(read["uncertainTail"], true);
    assert!(read["snapshot"].to_string().contains("durable"));
    assert!(!read["snapshot"].to_string().contains("lost tail"));
    assert_ne!(fresh.epoch(), epoch);
    assert!(fresh.snapshot().items.is_empty());
    drop(recorder);
}

#[tokio::test]
async fn durable_context_blobs_keep_sources_cursor_boundaries_and_workspace_isolation() {
    use crate::workbench::decode::details::{DetailDocument, DetailEntry, DetailSource};
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let options = options(dir.path());
    let root = options.root.clone();
    let recorder = Recorder::start(&hub, options).unwrap();
    let history = recorder.history.clone();
    let key = key();
    let document = DetailDocument {
        source: DetailSource::Request {
            client_request_index: None,
        },
        entries: (0..24)
            .map(|position| DetailEntry {
                section: "input".into(),
                position: Some(position),
                json_pointer: format!("/input/{position}"),
                children_separated: false,
                preview: format!("[redacted] <svg> {}", "context".repeat(500)),
                truncated: false,
                omitted: true,
            })
            .collect(),
        truncated: false,
        omitted: true,
        context_analysis: None,
    };
    hub.apply(Decoded {
        request_id: key.request_id,
        capture_seq: 42,
        received_at: Instant::now(),
        change: Change::Document { document },
    });
    publish(&hub, &key, "response");
    drop(recorder);
    let first = history
        .query(Query::Details {
            epoch: hub.epoch(),
            request: key.request_id,
            cursor: None,
            before: None,
            selection: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(first["entries"].as_array().unwrap().len(), 16);
    assert_eq!(first["omitted"], true);
    assert_eq!(first["entries"][0]["captureSeq"], 42);
    let cursor = first["nextCursor"].as_str().unwrap().to_string();
    let second = history
        .query(Query::Details {
            epoch: hub.epoch(),
            request: key.request_id,
            cursor: Some(cursor.clone()),
            before: None,
            selection: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(second["entries"].as_array().unwrap().len(), 8);
    assert_eq!(second["entries"][0]["position"], 16);
    assert!(second["nextCursor"].is_null());
    let other = history::History::start(root, "a".repeat(64), Uuid::new_v4()).unwrap();
    assert!(
        other.query(Query::List { cursor: None }).await.unwrap()["runs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        other
            .query(Query::Run { epoch: hub.epoch() })
            .await
            .unwrap_err(),
        HistoryError::NotFound
    );
    assert_eq!(
        history
            .query(Query::Details {
                epoch: hub.epoch(),
                request: Uuid::new_v4(),
                cursor: Some(cursor),
                before: None,
                selection: Default::default(),
            })
            .await
            .unwrap_err(),
        HistoryError::StaleCursor
    );
}

#[tokio::test]
async fn observed_context_summary_and_drilldown_survive_recording_with_the_same_calibration() {
    use crate::workbench::{
        capture::{self, Direction, Source, Transport},
        decode::{DecoderLimits, details::page::DetailSelection},
        observe::Observer,
    };
    use axum::body::Bytes;
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let recorder = Recorder::start(&hub, options(dir.path())).unwrap();
    let history = recorder.history.clone();
    let (sender, receiver) = capture::channel(2 * 1024 * 1024, 32);
    let observer = Observer::start(
        receiver,
        hub.clone(),
        RedactionPolicy::new(vec!["synthetic-context-secret".into()]).unwrap(),
        DecoderLimits::default(),
    )
    .unwrap();
    let request_id = Uuid::new_v4();
    let source = Source {
        request_id,
        direction: Direction::Request,
        transport: Transport::Http,
        content_type: "application/json".into(),
        content_encoding: String::new(),
    };
    let mut request = sender.stream(source.clone());
    let body = json!({"instructions":"base synthetic-context-secret","input":[
        {"role":"developer","content":"<skills>Read SKILL.md</skills>"},
        {"role":"user","content":"# AGENTS.md instructions for /fixture\n<INSTRUCTIONS>Use tests.</INSTRUCTIONS>\nActual request"},
        {"role":"user","content":"long original context ".repeat(5000)},
        {"type":"function_call","call_id":"read","name":"exec_command","arguments":"{\"cmd\":\"cat fixture.rs\"}"},
        {"type":"function_call_output","call_id":"read","output":"FILE_BODY synthetic-context-secret"},
        {"type":"function_call_output","call_id":"other","output":"TEST_OUTPUT"}
    ],"tools":[{"type":"function","name":"exec_command","parameters":{"type":"object"}}]});
    request.offer(Bytes::from(body.to_string()), Instant::now());
    request.finish();
    wait(|| !hub.snapshot().requests.is_empty());
    let initial = serde_json::to_value(hub.request_details(request_id, None).unwrap()).unwrap();
    assert_eq!(initial["contextSummary"]["calibrated"], false);
    assert_eq!(initial["contextSummary"]["coverage"]["complete"], true);
    assert_eq!(
        initial["contextSummary"]["parts"].as_array().unwrap().len(),
        7
    );
    assert_eq!(initial["truncated"], true);
    let before = hub.snapshot().view_seq;
    let response = json!({"type":"response.completed","response":{"id":"context-saved","status":"completed","output":[],"usage":{"input_tokens":101,"output_tokens":3,"total_tokens":104}}});
    let mut stream = sender.stream(Source {
        direction: Direction::Response,
        content_type: "text/event-stream".into(),
        ..source
    });
    stream.offer(Bytes::from(format!("data: {response}\n\n")), Instant::now());
    stream.finish();
    wait(|| {
        hub.snapshot()
            .responses
            .iter()
            .any(|r| r.response_id.as_deref() == Some("context-saved"))
    });
    let live = serde_json::to_value(hub.request_details(request_id, None).unwrap()).unwrap();
    assert_eq!(live["contextSummary"]["calibrated"], true);
    assert_eq!(live["contextSummary"]["tokens"], 101);
    assert_eq!(
        live["contextSummary"]["estimatedTokens"],
        initial["contextSummary"]["estimatedTokens"]
    );
    assert!(
        !hub.recording_checkpoint().details_partial,
        "preview clipping is separate from losing a document source"
    );
    drop(observer);
    drop(recorder);
    let saved = history
        .query(Query::Details {
            epoch: hub.epoch(),
            request: request_id,
            cursor: None,
            before: None,
            selection: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(saved["contextSummary"], live["contextSummary"]);
    let snapshot = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    let summary = serde_json::to_value(
        hub.snapshot().requests[0]
            .info
            .context_summary
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        snapshot["snapshot"]["requests"][0]["contextSummary"],
        summary
    );
    let earlier = history
        .query(Query::Details {
            epoch: hub.epoch(),
            request: request_id,
            cursor: None,
            before: Some(before),
            selection: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(earlier["contextSummary"]["calibrated"], false);
    let selection = DetailSelection {
        document: saved["selectedContext"].as_str().map(str::to_owned),
        category: Some("files".into()),
        item: None,
    };
    let file = history
        .query(Query::Details {
            epoch: hub.epoch(),
            request: request_id,
            cursor: None,
            before: None,
            selection,
        })
        .await
        .unwrap();
    assert_eq!(file["entries"].as_array().unwrap().len(), 1);
    assert!(
        file["entries"][0]["preview"]
            .as_str()
            .unwrap()
            .contains("FILE_BODY [已脱敏]")
    );
    assert!(!file.to_string().contains("synthetic-context-secret"));
    assert_eq!(sender.stats().dropped_chunks, 0);
}

#[tokio::test]
async fn saved_context_retains_request_scoped_gaps_without_degrading_unrelated_requests() {
    use crate::workbench::decode::{DiagnosticCode, ResponseStatus, details};
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(Default::default());
    let mut recorder_options = options(dir.path());
    // Each diagnostic also creates a reading notice. This case tests retained
    // capture scope; queue saturation has its own fault-injection coverage.
    recorder_options.queue_events = 1024;
    let recorder = Recorder::start(&hub, recorder_options).unwrap();
    let history = recorder.history.clone();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let policy = RedactionPolicy::new(vec![]).unwrap();
    let apply = |request_id, change| {
        hub.apply(Decoded {
            request_id,
            capture_seq: 1,
            received_at: Instant::now(),
            change,
        })
    };
    for id in [a, b] {
        apply(
            id,
            Change::Document {
                document: details::request(&json!({"input":"hello"}), None, &policy),
            },
        );
        apply(
            id,
            Change::Document {
                document: details::response(&json!({"id":id,"output":[]}), &policy),
            },
        );
        apply(
            id,
            Change::Response {
                response_id: Some(id.to_string()),
                status: ResponseStatus::Completed,
                model: None,
                usage: details::usage(&json!({"input_tokens":101})),
            },
        );
    }
    apply(
        a,
        Change::Diagnostic {
            code: DiagnosticCode::ObservationGap,
        },
    );
    for _ in 0..128 {
        apply(
            Uuid::new_v4(),
            Change::Diagnostic {
                code: DiagnosticCode::InvalidJson,
            },
        );
    }
    let expected_a = serde_json::to_value(hub.request_details(a, None).unwrap()).unwrap();
    let expected_b = serde_json::to_value(hub.request_details(b, None).unwrap()).unwrap();
    assert_eq!(expected_a["contextSummary"]["calibrated"], false);
    assert_eq!(expected_b["contextSummary"]["calibrated"], true);
    drop(recorder);
    let run = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    assert_eq!(run["gaps"], json!([]));
    assert_eq!(run["issues"], json!([]));
    assert_eq!(run["snapshot"]["detailsPartialRequests"], json!([a]));
    for (id, expected) in [(a, expected_a), (b, expected_b)] {
        let saved = history
            .query(Query::Details {
                epoch: hub.epoch(),
                request: id,
                cursor: None,
                before: None,
                selection: Default::default(),
            })
            .await
            .unwrap();
        assert_eq!(
            saved["contextSummary"], expected["contextSummary"],
            "issues: {}, conflict: {}",
            saved["captureIssues"], saved["conflict"]
        );
        if id == b {
            assert_eq!(saved["truncated"], false);
        }
    }
}

#[test]
fn old_checkpoint_coverage_falls_back_and_replay_retention_never_forgets_gaps() {
    let hub = LiveHub::new(Default::default());
    let mut value = serde_json::to_value(hub.recording_checkpoint()).unwrap();
    value["snapshot"]
        .as_object_mut()
        .unwrap()
        .remove("detailsPartialRequests");
    value["detailsPartial"] = serde_json::json!(true);
    let old: replay::Checkpoint = serde_json::from_value(value).unwrap();
    assert!(old.details_partial && old.details_partial_requests().is_none());
    let mut checkpoint = hub.recording_checkpoint();
    for index in 0..834 {
        let request_id = Uuid::from_u128(index + 1);
        checkpoint.document(replay::Document { request_id, capture_seq: 1, document: serde_json::json!({"source":{"kind":"request","clientRequestIndex":null},"entries":[],"truncated":false,"omitted":false}) });
        if index == 512 {
            assert_eq!(
                checkpoint.details_partial_requests().unwrap(),
                [Uuid::from_u128(1)]
            );
        }
    }
    assert_eq!(checkpoint.documents.len(), 512);
    assert!(checkpoint.details_partial && checkpoint.details_partial_requests().is_none());
}

#[tokio::test]
async fn recorder_exit_has_a_deadline_and_a_late_worker_cannot_mark_it_clean() {
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let options = options(dir.path());
    let faults = options.faults.clone();
    let mut recorder = Recorder::start(&hub, options).unwrap();
    let history = recorder.history.clone();
    wait(|| hub.recorder_status().state == "saved");
    let (release, pause) = mpsc::channel();
    *faults.pause_gate.lock().unwrap() = Some(pause);
    wait(|| faults.pause_active.load(Ordering::Acquire));
    // Observe actual worker completion after releasing the stall, not an
    // assumed scheduler/fsync delay. Drop still waits on its real done channel.
    let worker = recorder.thread.take().unwrap();
    publish(&hub, &key(), "tail after stall");
    let started = Instant::now();
    drop(recorder);
    assert!(started.elapsed() < Duration::from_millis(2300));
    assert_eq!(hub.recorder_status().error, Some("shutdown_timeout"));
    assert!(!worker.is_finished());
    let active = history
        .query(Query::Status { epoch: hub.epoch() })
        .await
        .unwrap();
    assert_eq!(active["run"]["state"], "active");
    release.send(()).unwrap();
    wait(|| worker.is_finished());
    worker.join().unwrap();
    let read = history
        .query(Query::Run { epoch: hub.epoch() })
        .await
        .unwrap();
    assert_eq!(read["run"]["state"], "unclean");
    assert_eq!(read["uncertainTail"], true);
    assert!(
        !dir.path()
            .join("history/runs")
            .join(hub.epoch().to_string())
            .join("lifecycle.json")
            .try_exists()
            .unwrap()
    );
}
