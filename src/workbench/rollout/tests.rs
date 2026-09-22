use super::*;
use crate::workbench::decode::request::{PurposeBasis, RequestInfo, RequestPurpose};
use crate::workbench::decode::{Change, Decoded};
use crate::workbench::live::LiveLimits;
use serde_json::json;
use std::io::Write;
use std::time::Instant;

mod native_patches;
mod native_tools;

#[test]
fn archived_sources_and_moves_keep_the_same_offsets_and_do_not_duplicate_users() {
    let (dir, hub, mut reader, thread, path) = setup();
    target(&hub, thread, "turn-a");
    append(&path, &message(thread, "turn-a", "first", "before archive"));
    reader.tick(&hub);
    let first = hub.snapshot().user_messages()[0].source.clone();
    let archive = dir.path().join("archived_sessions");
    std::fs::create_dir(&archive).unwrap();
    let moved = archive.join(path.file_name().unwrap());
    std::fs::rename(path, &moved).unwrap();
    append(
        &moved,
        &message(thread, "turn-a", "second", "after archive"),
    );
    for _ in 0..3 {
        reader.tick(&hub);
    }
    let snap = hub.snapshot();
    let users = snap.user_messages();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0].source, first);
    assert_eq!(users[1].source.source_ref, first.source_ref);
    let other = LiveHub::new(LiveLimits::default());
    target(&other, thread, "turn-a");
    let mut fresh = Reader::new(dir.path(), policy()).unwrap();
    fresh.tick(&other);
    assert_eq!(other.snapshot().user_messages().len(), 2);
    assert_eq!(
        other.snapshot().user_messages()[1].source.byte_offset,
        users[1].source.byte_offset
    );
}

fn policy() -> Arc<RedactionPolicy> {
    RedactionPolicy::new(vec!["private-fixture-value".into()]).unwrap()
}
fn target(hub: &LiveHub, thread: Uuid, turn: &str) {
    hub.apply(Decoded {
        request_id: Uuid::new_v4(),
        capture_seq: 1,
        received_at: Instant::now(),
        change: Change::Request {
            info: RequestInfo {
                client_request_index: None,
                requested_model: None,
                codex_thread_id: Some(thread),
                codex_turn_id: Some(turn.into()),
                purpose: RequestPurpose::Conversation,
                purpose_basis: PurposeBasis::CodexTurnMetadata,
            },
        },
    });
}
fn session(thread: Uuid) -> Value {
    json!({"ordinal":0,"type":"session_meta","payload":{"id":thread,"thread_source":"user","source":"cli"}})
}
fn message(thread: Uuid, turn: &str, item: &str, text: &str) -> Value {
    json!({"ordinal":7,"type":"event_msg","payload":{"type":"item_completed","thread_id":thread,"turn_id":turn,"item":{"type":"UserMessage","id":item,"content":[{"type":"text","text":text}]}}})
}
fn append(path: &Path, value: &Value) {
    writeln!(
        std::fs::OpenOptions::new().append(true).open(path).unwrap(),
        "{value}"
    )
    .unwrap();
}
fn setup() -> (
    tempfile::TempDir,
    Arc<LiveHub>,
    Reader,
    Uuid,
    std::path::PathBuf,
) {
    let directory = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    hub.enable_user_reader();
    let thread = Uuid::new_v4();
    let folder = directory.path().join("sessions/2026/09/19");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join(format!("rollout-synthetic-{thread}.jsonl"));
    std::fs::write(&path, format!("{}\n", session(thread))).unwrap();
    let reader = Reader::new(directory.path(), policy()).unwrap();
    (directory, hub, reader, thread, path)
}

#[test]
fn accepts_only_native_user_item_evidence_and_preserves_identical_submissions() {
    let (_dir, hub, mut reader, thread, path) = setup();
    target(&hub, thread, "turn-a");
    append(
        &path,
        &json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"injected context"}]}}),
    );
    append(&path, &message(thread, "turn-a", "item-a", "中文\n同一句"));
    append(&path, &message(thread, "turn-b", "item-b", "中文\n同一句"));
    reader.tick(&hub);
    assert_eq!(hub.snapshot().user_messages().len(), 1);
    // A future turn's event can arrive before its network metadata. Keep bounded
    // pending evidence, but expose it only after the explicit target arrives.
    target(&hub, thread, "turn-b");
    reader.tick(&hub);
    reader.tick(&hub);
    let snapshot = hub.snapshot();
    let users = snapshot.user_messages();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0].text, users[1].text);
    assert_ne!(users[0].key, users[1].key);
    assert_eq!(users[0].source.ordinal, Some(7));
    assert!(users[0].source.byte_offset > 0);
    let serialized = serde_json::to_string(&users).unwrap();
    assert!(!serialized.contains(path.to_str().unwrap()));
    assert!(!serialized.contains("injected"));
    // Replayed native item is not a second submission, even at another offset.
    append(&path, &message(thread, "turn-a", "item-a", "中文\n同一句"));
    reader.tick(&hub);
    assert_eq!(hub.snapshot().user_messages().len(), 2);
}

#[test]
fn partial_lines_bad_json_and_overlong_records_do_not_poison_the_next_user() {
    let (_dir, hub, mut reader, thread, path) = setup();
    target(&hub, thread, "turn-a");
    let value = message(thread, "turn-a", "item-a", "中\n文").to_string();
    let split = value.len() / 2;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(&value.as_bytes()[..split]).unwrap();
    reader.tick(&hub);
    assert!(hub.snapshot().user_messages().is_empty());
    file.write_all(&value.as_bytes()[split..]).unwrap();
    file.write_all(b"\ninvalid\n").unwrap();
    file.write_all(&vec![b'x'; 1024 * 1024 + 1]).unwrap();
    file.write_all(b"\n").unwrap();
    append(
        &path,
        &message(thread, "turn-a", "item-b", "after invalid line"),
    );
    for _ in 0..8 {
        reader.tick(&hub);
    }
    assert_eq!(hub.snapshot().user_messages().len(), 2);
    let codes: Vec<_> = hub
        .snapshot()
        .user_capture
        .diagnostics
        .iter()
        .map(|d| d.code)
        .collect();
    assert!(codes.contains(&UserIssue::InvalidLine));
    assert!(codes.contains(&UserIssue::LineTooLarge));
    assert!(reader.tails[0].lines.offset > 1024 * 1024);
}

#[test]
fn session_and_event_identity_must_both_match_and_conflicts_cannot_overwrite() {
    let (_dir, hub, mut reader, thread, path) = setup();
    target(&hub, thread, "turn-a");
    append(
        &path,
        &message(Uuid::new_v4(), "turn-a", "wrong", "wrong thread"),
    );
    append(&path, &message(thread, "turn-a", "item-a", "original"));
    append(
        &path,
        &message(thread, "turn-a", "item-a", "conflicting replacement"),
    );
    reader.tick(&hub);
    assert_eq!(hub.snapshot().user_messages().len(), 1);
    assert_eq!(hub.snapshot().user_messages()[0].text, "original");
    assert!(
        hub.snapshot()
            .user_capture
            .diagnostics
            .iter()
            .any(|d| d.code == UserIssue::IdentityConflict)
    );
    let (_other, other_hub, mut other_reader, other_thread, other_path) = setup();
    target(&other_hub, other_thread, "turn-a");
    std::fs::write(
        &other_path,
        format!(
            "{}\n{}\n",
            session(thread),
            message(other_thread, "turn-a", "item-a", "untrusted header")
        ),
    )
    .unwrap();
    other_reader.tick(&other_hub);
    assert!(other_hub.snapshot().user_messages().is_empty());
}

#[test]
fn safe_preview_omits_non_text_and_redacts_before_entering_live_state() {
    let (_dir, hub, mut reader, thread, path) = setup();
    target(&hub, thread, "turn-a");
    let mut value = message(
        thread,
        "turn-a",
        "item-a",
        "<script>literal</script> private-fixture-value\nBearer fixture",
    );
    value["payload"]["item"]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"local_image","path":"/private/synthetic.png"}));
    append(&path, &value);
    append(
        &path,
        &message(thread, "turn-a", "item-b", &"中".repeat(30000)),
    );
    reader.tick(&hub);
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.user_messages().len(), 2);
    assert!(snapshot.user_messages()[0].omitted);
    assert!(snapshot.user_messages()[1].truncated);
    assert!(snapshot.user_messages()[1].text.len() <= USER_TEXT_BYTES);
    let serialized = serde_json::to_string(&snapshot).unwrap();
    assert!(!serialized.contains("private-fixture-value"));
    assert!(!serialized.contains("Bearer fixture"));
    assert!(!serialized.contains("/private/synthetic.png"));
    assert!(serialized.contains("已脱敏"));
}

#[test]
fn no_symlink_file_or_directory_is_followed_and_nonregular_files_cannot_block() {
    use std::os::unix::fs::symlink;
    let (_dir, hub, mut reader, thread, path) = setup();
    target(&hub, thread, "turn-a");
    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().join(path.file_name().unwrap());
    std::fs::write(
        &outside_path,
        format!(
            "{}\n{}\n",
            session(thread),
            message(thread, "turn-a", "item-a", "outside")
        ),
    )
    .unwrap();
    std::fs::remove_file(&path).unwrap();
    symlink(&outside_path, &path).unwrap();
    reader.tick(&hub);
    assert!(hub.snapshot().user_messages().is_empty());
    std::fs::remove_file(&path).unwrap();
    let day = path.parent().unwrap();
    std::fs::remove_dir(day).unwrap();
    symlink(outside.path(), day).unwrap();
    reader.tick(&hub);
    assert!(hub.snapshot().user_messages().is_empty());
    let dir = files::root(outside.path()).unwrap();
    let fifo = outside.path().join("fifo.jsonl");
    use std::os::unix::ffi::OsStrExt;
    let raw = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
    assert!(files::child(&dir, std::ffi::OsStr::new("fifo.jsonl"), false).is_err());
    assert!(files::child(&dir, std::ffi::OsStr::new("../escape"), false).is_err());
}

#[test]
fn truncation_and_replacement_are_marked_without_joining_new_content() {
    for replace in [false, true] {
        let (_dir, hub, mut reader, thread, path) = setup();
        target(&hub, thread, "turn-a");
        append(&path, &message(thread, "turn-a", "item-a", "original"));
        reader.tick(&hub);
        if replace {
            std::fs::remove_file(&path).unwrap();
        }
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                session(thread),
                message(
                    thread,
                    "turn-a",
                    "item-b",
                    "replacement with longer content"
                )
            ),
        )
        .unwrap();
        reader.tick(&hub);
        assert_eq!(hub.snapshot().user_messages().len(), 1);
        assert!(
            hub.snapshot()
                .user_capture
                .diagnostics
                .iter()
                .any(|d| d.code == UserIssue::SourceChanged)
        );
    }
}

#[test]
fn unmatched_records_are_bounded_and_cannot_create_user_bubbles() {
    let (_dir, hub, mut reader, thread, path) = setup();
    target(&hub, thread, "current");
    for index in 0..140 {
        append(
            &path,
            &message(
                thread,
                "old-turn",
                &format!("item-{index}"),
                &"x".repeat(12000),
            ),
        );
    }
    for _ in 0..10 {
        reader.tick(&hub);
    }
    assert!(reader.pending.len() <= 128);
    assert!(reader.pending.iter().map(|r| r.text.len()).sum::<usize>() <= PENDING_BYTES);
    assert!(hub.snapshot().user_messages().is_empty());
    assert!(
        hub.snapshot()
            .user_capture
            .diagnostics
            .iter()
            .any(|d| d.code == UserIssue::Capacity)
    );
}

#[test]
fn scan_budget_resumes_and_source_reader_does_not_require_a_web_handler() {
    let (dir, hub, _reader, thread, path) = setup();
    let root = files::root(dir.path()).unwrap();
    let mut scan = files::Scan::new(&root).unwrap();
    let mut found = 0;
    for _ in 0..10 {
        if scan.step(1, |_, _| found += 1).unwrap() {
            break;
        }
    }
    assert_eq!(found, 1);
    target(&hub, thread, "turn-a");
    let _reader = RolloutReader::start(dir.path(), hub.clone(), policy()).unwrap();
    append(
        &path,
        &message(thread, "turn-a", "item-a", "asynchronous user"),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while hub.snapshot().user_messages().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(hub.snapshot().user_messages().len(), 1);
}

#[tokio::test]
async fn user_events_share_snapshot_cursor_and_duplicate_records_do_not_publish_again() {
    let (_dir, hub, mut reader, thread, path) = setup();
    target(&hub, thread, "turn-a");
    let before = hub.snapshot();
    let mut stream = hub.subscribe(hub.epoch(), before.view_seq).unwrap();
    append(&path, &message(thread, "turn-a", "item-a", "actual user"));
    reader.tick(&hub);
    let published = stream.recv().await.unwrap();
    let event: Value = serde_json::from_str(&published.json).unwrap();
    assert_eq!(event["kind"], "item.replace");
    assert!(
        event["requestId"].is_null(),
        "rollout must not invent a network request ID"
    );
    assert_eq!(event["item"]["author"]["role"], "user");
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.view_seq, before.view_seq + 1);
    assert_eq!(snapshot.user_messages()[0].text, "actual user");
    append(&path, &message(thread, "turn-a", "item-a", "actual user"));
    reader.tick(&hub);
    assert_eq!(hub.snapshot().view_seq, snapshot.view_seq);
}
