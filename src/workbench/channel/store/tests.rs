use super::*;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

fn key(message_id: i64) -> DeliveryKey {
    DeliveryKey {
        bot_id: 123,
        chat_id: 456,
        message_id,
    }
}
fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let store = Store::open(&dir.path().join("channel")).unwrap();
    (dir, store)
}
fn assert_reopen_fails(path: &Path) {
    assert!(Store::open(path).is_err());
}

#[test]
fn rejected_metadata_survives_restart_without_becoming_a_task() {
    let (dir, mut store) = setup();
    store.reject(123, 7, Some(key(9))).unwrap();
    store.reject(123, 8, None).unwrap();
    let bytes = store.bytes;
    store.reject(123, 7, Some(key(9))).unwrap();
    store.reject(123, 999, Some(key(9))).unwrap();
    assert_eq!(store.bytes, bytes);
    assert!(store.recover().unwrap().is_empty());
    drop(store);
    let mut store = Store::open(&dir.path().join("channel")).unwrap();
    assert!(store.is_rejected(123, 7, None).unwrap());
    assert!(store.is_rejected(123, 8, None).unwrap());
    assert!(store.is_rejected(123, 999, Some(key(9))).unwrap());
    assert!(!store.is_rejected(124, 7, None).unwrap());
    assert_eq!(
        store.admit(key(10), 8, Uuid::from_u128(1)),
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        store.admit(key(9), 999, Uuid::from_u128(2)),
        Ok(Admission::Duplicate)
    );
    assert!(store.recover().unwrap().is_empty());
    let text = fs::read_to_string(dir.path().join("channel").join(JOURNAL)).unwrap();
    assert!(!text.contains("task"));
    assert!(!text.contains("user_id"));
    for line in text.lines() {
        let line: Line = serde_json::from_str(line).unwrap();
        assert_eq!(line.record.version, 4);
        // The previous reader's exact version gate rejects every new row.
        assert_ne!(line.record.version, 1);
    }
}

fn fixture_line(record: Record) -> Vec<u8> {
    let digest = blake3::hash(&serde_json::to_vec(&record).unwrap())
        .to_hex()
        .to_string();
    let mut bytes = serde_json::to_vec(&Line { record, digest }).unwrap();
    bytes.push(b'\n');
    bytes
}
#[test]
fn v1_receipts_remain_readable_but_cannot_claim_rejection_schema() {
    let (dir, store) = setup();
    let path = dir.path().join("channel");
    drop(store);
    fs::write(
        path.join(JOURNAL),
        fixture_line(Record {
            version: 1,
            sequence: 1,
            event: Event::Received {
                key: key(1),
                update_id: 1,
                task: Uuid::from_u128(1),
            },
        }),
    )
    .unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store.admit(key(1), 2, Uuid::from_u128(2)),
        Ok(Admission::Duplicate)
    );
    store.reject(123, 3, Some(key(3))).unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert!(store.is_rejected(123, 3, Some(key(3))).unwrap());
    drop(store);
    for version in [1, 5] {
        fs::write(
            path.join(JOURNAL),
            fixture_line(Record {
                version,
                sequence: 1,
                event: Event::Rejected {
                    bot_id: 123,
                    update_id: 1,
                    message_key: Some(key(1)),
                },
            }),
        )
        .unwrap();
        assert!(matches!(Store::open(&path), Err(Failure::Corrupt)));
    }
}

#[test]
fn rejected_identity_validation_and_combined_cap_fail_closed() {
    let (_dir, mut store) = setup();
    for (bot, update, key) in [
        (0, 1, None),
        (123, -1, None),
        (124, 1, Some(key(1))),
        (123, 1, Some(key(0))),
    ] {
        assert_eq!(store.reject(bot, update, key), Err(Failure::InvalidInput));
        assert_eq!(
            store.is_rejected(bot, update, key),
            Err(Failure::InvalidInput)
        );
    }
    // Exercise the actual combined boundary without thousands of redundant
    // disk flushes; persistence/reopen is covered by the preceding test.
    for id in 0..MAX_TASKS - 1 {
        store.rejected_updates.insert((123, id as i64));
    }
    assert_eq!(
        store.admit(key(1), MAX_TASKS as i64, Uuid::from_u128(1)),
        Ok(Admission::Fresh)
    );
    assert_eq!(
        store.reject(123, MAX_TASKS as i64 + 1, None),
        Err(Failure::Capacity)
    );
    assert_eq!(
        store.admit(key(2), MAX_TASKS as i64 + 2, Uuid::from_u128(2)),
        Err(Failure::Capacity)
    );
    assert_eq!(store.reject(123, 0, None), Ok(()));
    assert_eq!(
        store.admit(key(2), 0, Uuid::from_u128(2)),
        Ok(Admission::Duplicate)
    );
}

#[test]
fn receipts_survive_reopen_and_deduplicate_message_not_update() {
    let (dir, mut store) = setup();
    let task = Uuid::new_v4();
    assert_eq!(store.admit(key(1), 20, task), Ok(Admission::Fresh));
    assert_eq!(
        store.admit(key(1), 21, Uuid::new_v4()),
        Ok(Admission::Duplicate)
    );
    store.mark_submitting(task, Uuid::new_v4()).unwrap();
    drop(store);
    let mut store = Store::open(&dir.path().join("channel")).unwrap();
    assert_eq!(store.recover().unwrap(), vec![(task, Status::Submitting)]);
    assert_eq!(
        store.admit(key(1), 99, Uuid::new_v4()),
        Ok(Admission::Duplicate)
    );
    // Recovery reports the uncertain attempt and cannot re-submit it.
    assert_eq!(
        store.mark_submitting(task, Uuid::new_v4()),
        Err(Failure::InvalidTransition)
    );
    store.close(task).unwrap();
    drop(store);
    let mut store = Store::open(&dir.path().join("channel")).unwrap();
    assert_eq!(store.recover().unwrap(), vec![(task, Status::Closed)]);
    assert_eq!(
        store.admit(key(1), 100, Uuid::new_v4()),
        Ok(Admission::Duplicate)
    );
}
const CRASH_TASK: Uuid = Uuid::from_u128(0xa11ce);
const CRASH_CLIENT: Uuid = Uuid::from_u128(0xc11e17);
const CRASH_EXIT: i32 = 73;

#[test]
fn durable_submission_crash_child() {
    // Ordinary suite execution is inert. Only the isolated parent below
    // sets this marker and selects this exact helper in a new process.
    if std::env::var("CODEX_CHANNEL_STORE_CRASH_CHILD").as_deref() != Ok("1") {
        return;
    }
    let root = std::env::var_os("CODEX_CHANNEL_STORE_CRASH_ROOT")
        .expect("synthetic crash root must be configured");
    let mut store = Store::open(Path::new(&root)).unwrap();
    assert_eq!(store.admit(key(7), 19, CRASH_TASK), Ok(Admission::Fresh));
    store.mark_submitting(CRASH_TASK, CRASH_CLIENT).unwrap();
    store.reject(123, 21, Some(key(8))).unwrap();
    store
        .admit_callback(123, 23, "crash-query", CRASH_TASK, Uuid::from_u128(0xc411))
        .unwrap();
    store
        .admit_question_reply(key(9), 25, CRASH_TASK, Uuid::from_u128(0xc411))
        .unwrap();
    // All APIs have fsynced. Exit immediately without unwinding or running
    // Store/File destructors, as opposed to merely dropping/reopening it.
    std::process::exit(CRASH_EXIT);
}

#[test]
fn durable_submission_survives_abrupt_process_exit_without_replay() {
    let sandbox = tempfile::tempdir().unwrap();
    fs::set_permissions(sandbox.path(), fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["home", "codex-home", "tmp"] {
        let path = sandbox.path().join(name);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let root = sandbox.path().join("channel");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "workbench::channel::store::tests::durable_submission_crash_child",
            "--test-threads=1",
        ])
        .env_clear()
        .env("HOME", sandbox.path().join("home"))
        .env("USERPROFILE", sandbox.path().join("home"))
        .env("CODEX_HOME", sandbox.path().join("codex-home"))
        .env("TMPDIR", sandbox.path().join("tmp"))
        .env("CODEX_CHANNEL_STORE_CRASH_CHILD", "1")
        .env("CODEX_CHANNEL_STORE_CRASH_ROOT", &root)
        .current_dir(sandbox.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("synthetic crash child must run");
    assert_eq!(status.code(), Some(CRASH_EXIT));
    let before = fs::read(root.join(JOURNAL)).unwrap();
    let mut store = Store::open(&root).unwrap();
    assert_eq!(
        store.admit_question_reply(key(9), 26, CRASH_TASK, Uuid::from_u128(0xc411)),
        Ok(Admission::Duplicate)
    );
    assert!(store.is_rejected(123, 21, Some(key(8))).unwrap());
    assert_eq!(
        store.admit_callback(123, 24, "crash-query", CRASH_TASK, Uuid::from_u128(0xc411)),
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        store.admit(key(8), 22, Uuid::from_u128(0xa11d0)),
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        store.recover().unwrap(),
        vec![(CRASH_TASK, Status::Submitting)]
    );
    assert_eq!(
        store.admit(key(7), 20, Uuid::from_u128(0xa11cf)),
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        store.mark_submitting(CRASH_TASK, CRASH_CLIENT),
        Err(Failure::InvalidTransition)
    );
    assert_eq!(
        store.recover().unwrap(),
        vec![(CRASH_TASK, Status::Submitting)]
    );
    assert_eq!(fs::read(root.join(JOURNAL)).unwrap(), before);
}

#[test]
fn validates_input_transitions_and_client_uniqueness() {
    let (_dir, mut store) = setup();
    let task = Uuid::new_v4();
    assert_eq!(store.admit(key(0), 0, task), Err(Failure::InvalidInput));
    assert_eq!(store.admit(key(1), -1, task), Err(Failure::InvalidInput));
    assert_eq!(
        store.admit(key(1), 0, Uuid::nil()),
        Err(Failure::InvalidInput)
    );
    assert_eq!(store.close(task), Err(Failure::InvalidTransition));
    store.admit(key(1), 0, task).unwrap();
    assert_eq!(
        store.admit(key(2), 1, task),
        Err(Failure::InvalidTransition)
    );
    let client = Uuid::new_v4();
    store.mark_submitting(task, client).unwrap();
    let second = Uuid::new_v4();
    store.admit(key(2), 1, second).unwrap();
    assert_eq!(
        store.mark_submitting(second, client),
        Err(Failure::InvalidTransition)
    );
    store.close(second).unwrap();
    assert_eq!(store.close(second), Err(Failure::InvalidTransition));
}
#[test]
fn lock_is_exclusive_and_files_are_private() {
    let (dir, store) = setup();
    let root = dir.path().join("channel");
    assert!(matches!(Store::open(&root), Err(Failure::Busy)));
    assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
    for name in [LOCK, JOURNAL] {
        assert_eq!(fs::metadata(root.join(name)).unwrap().mode() & 0o777, 0o600);
    }
    drop(store);
    assert!(Store::open(&root).is_ok());
}
#[test]
fn torn_corrupt_unknown_and_out_of_order_lines_fail_closed() {
    for corruption in 0..6 {
        let (dir, mut store) = setup();
        store.admit(key(1), 1, Uuid::new_v4()).unwrap();
        drop(store);
        let path = dir.path().join("channel").join(JOURNAL);
        let mut bytes = fs::read(&path).unwrap();
        match corruption {
            0 => {
                bytes.pop();
            }
            1 => {
                bytes.extend_from_slice(b"{\"record\":");
            }
            2 => {
                bytes.extend_from_slice(b"\n");
            }
            3 => {
                let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                value["unexpected"] = true.into();
                bytes = serde_json::to_vec(&value).unwrap();
                bytes.push(b'\n');
            }
            4 => {
                let copy = bytes.clone();
                bytes.extend(copy);
            }
            _ => {
                let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                value["record"]["event"]["key"]["chat_id"] = 987.into();
                bytes = serde_json::to_vec(&value).unwrap();
                bytes.push(b'\n');
            }
        }
        fs::write(&path, bytes).unwrap();
        assert_reopen_fails(&dir.path().join("channel"));
    }
}
#[test]
fn replacement_or_truncation_permanently_disables_open_store() {
    for name in [JOURNAL, LOCK, "root", "truncate"] {
        let (dir, mut store) = setup();
        store.admit(key(1), 1, Uuid::new_v4()).unwrap();
        let root = dir.path().join("channel");
        if name == "root" {
            fs::rename(&root, dir.path().join("old")).unwrap();
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        } else if name == "truncate" {
            fs::write(root.join(JOURNAL), b"").unwrap();
        } else {
            fs::rename(root.join(name), root.join("old")).unwrap();
            fs::write(root.join(name), b"").unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(store.admit(key(2), 2, Uuid::new_v4()).is_err());
        assert_eq!(
            store.admit(key(3), 3, Uuid::new_v4()),
            Err(Failure::Poisoned)
        );
    }
}
#[test]
fn journal_lock_and_token_reject_symlinks_hardlinks_and_public_modes() {
    for name in [JOURNAL, LOCK, TOKEN] {
        for attack in ["symlink", "hardlink", "permissions"] {
            let (dir, mut store) = setup();
            store.save_token("123:synthetic_only").unwrap();
            let root = dir.path().join("channel");
            drop(store);
            let target = root.join(name);
            if attack == "permissions" {
                fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
            } else {
                let outside = dir.path().join("outside");
                fs::rename(&target, &outside).unwrap();
                if attack == "symlink" {
                    symlink(&outside, &target).unwrap();
                } else {
                    fs::hard_link(&outside, &target).unwrap();
                }
            }
            if name == TOKEN {
                let mut reopened = Store::open(&root).unwrap();
                assert!(reopened.token().is_err());
                assert!(reopened.save_token("123:replacement").is_err());
                assert!(reopened.remove_token().is_err());
            } else {
                assert_reopen_fails(&root);
            }
        }
    }
}
#[test]
fn root_symlinks_and_public_directory_are_rejected() {
    let (dir, store) = setup();
    drop(store);
    symlink(dir.path().join("channel"), dir.path().join("link")).unwrap();
    assert_reopen_fails(&dir.path().join("link"));
    fs::set_permissions(
        dir.path().join("channel"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_reopen_fails(&dir.path().join("channel"));
}
#[test]
fn root_creation_requires_a_private_parent() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert_reopen_fails(&dir.path().join("channel"));
    assert!(!dir.path().join("channel").exists());
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(Store::open(&dir.path().join("channel")).is_ok());
}
#[test]
fn deleted_journal_and_interrupted_initialization_never_reset_receipts() {
    let (dir, store) = setup();
    drop(store);
    fs::remove_file(dir.path().join("channel").join(JOURNAL)).unwrap();
    assert_reopen_fails(&dir.path().join("channel"));
    let fresh = dir.path().join("incomplete");
    let directory = Directory::root(&fresh).unwrap();
    directory.open(LOCK, true).unwrap();
    assert_reopen_fails(&fresh);
}
#[test]
fn append_io_failure_poisoning_prevents_later_admission() {
    let (_dir, mut store) = setup();
    // A read-only descriptor deterministically injects an append failure,
    // with the same valid inode and without touching real user storage.
    store.journal = store.dir.open(JOURNAL, false).unwrap();
    assert_eq!(
        store.admit(key(1), 1, Uuid::new_v4()),
        Err(Failure::Unavailable)
    );
    assert_eq!(
        store.admit(key(2), 2, Uuid::new_v4()),
        Err(Failure::Poisoned)
    );
}
#[test]
fn bounded_task_and_byte_capacity_do_not_evict_receipts() {
    let (_dir, mut store) = setup();
    // Build a valid synthetic journal in one fsync to keep the capacity test
    // fast; reopen still validates every receipt and its digest.
    let mut bytes = Vec::new();
    for index in 1..=MAX_TASKS {
        let record = Record {
            version: 1,
            sequence: index as u64,
            event: Event::Received {
                key: key(index as i64),
                update_id: index as i64,
                task: Uuid::new_v4(),
            },
        };
        let digest = blake3::hash(&serde_json::to_vec(&record).unwrap())
            .to_hex()
            .to_string();
        bytes.extend(serde_json::to_vec(&Line { record, digest }).unwrap());
        bytes.push(b'\n');
    }
    store.journal.write_all(&bytes).unwrap();
    store.journal.sync_all().unwrap();
    let path = store.dir.path.clone();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store.admit(key(MAX_TASKS as i64 + 1), 9999, Uuid::new_v4()),
        Err(Failure::Capacity)
    );
    assert_eq!(
        store.admit(key(1), 9999, Uuid::new_v4()),
        Ok(Admission::Duplicate)
    );
    drop(store);
    let directory = Directory::read_only(&path).unwrap();
    directory
        .append(JOURNAL)
        .unwrap()
        .set_len(MAX_BYTES as u64 + 1)
        .unwrap();
    assert!(matches!(Store::open(&path), Err(Failure::Capacity)));
}
#[test]
fn credentials_are_separate_atomic_private_and_errors_are_static() {
    let (dir, mut store) = setup();
    assert!(store.token().unwrap().is_none());
    store.save_token("123:synthetic_only-A").unwrap();
    let path = dir.path().join("channel").join(TOKEN);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    let first = fs::metadata(&path).unwrap().ino();
    store.save_token("123:synthetic_only-B").unwrap();
    assert_ne!(first, fs::metadata(&path).unwrap().ino());
    drop(store);
    let mut store = Store::open(&dir.path().join("channel")).unwrap();
    assert_eq!(
        store.token().unwrap().as_deref(),
        Some("123:synthetic_only-B")
    );
    assert!(
        fs::read(dir.path().join("channel").join(JOURNAL))
            .unwrap()
            .is_empty()
    );
    for invalid in [
        "",
        "0:secret",
        "123:",
        "123:secret\n",
        "https://token",
        "123:非ASCII",
        "9223372036854775808:secret",
        &format!("123:{}", "a".repeat(TOKEN_LIMIT)),
    ] {
        let error = store.save_token(invalid).unwrap_err();
        assert_eq!(error, Failure::InvalidInput);
        assert_eq!(error.to_string(), "channel store invalid input");
    }
    fs::write(&path, [0xff]).unwrap();
    assert_eq!(store.token(), Err(Failure::Corrupt));
    store.save_token("123:synthetic_repaired").unwrap();
    fs::write(&path, vec![b'a'; TOKEN_LIMIT + 1]).unwrap();
    assert_eq!(store.token(), Err(Failure::Unavailable));
    store.remove_token().unwrap();
    store.remove_token().unwrap();
    assert!(store.token().unwrap().is_none());
    assert_eq!(fs::read_dir(dir.path().join("channel")).unwrap().count(), 2);
}

fn submitting(store: &mut Store) -> (Uuid, Uuid) {
    let task = Uuid::from_u128(1);
    let interaction = Uuid::from_u128(2);
    store.admit(key(1), 1, task).unwrap();
    store.mark_submitting(task, Uuid::from_u128(3)).unwrap();
    (task, interaction)
}

#[test]
fn callback_receipts_deduplicate_both_identities_without_learning_conflicts_or_replaying() {
    let (dir, mut store) = setup();
    let (task, interaction) = submitting(&mut store);
    assert_eq!(
        store.admit_callback(123, 10, "query-a", task, interaction),
        Ok(Admission::Fresh)
    );
    let before = fs::read(dir.path().join("channel").join(JOURNAL)).unwrap();
    assert_eq!(
        store.admit_callback(123, 11, "query-a", task, interaction),
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        store.admit_callback(123, 10, "query-b", task, interaction),
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        fs::read(dir.path().join("channel").join(JOURNAL)).unwrap(),
        before
    );
    // Neither otherwise-new identity on the preceding duplicates is learned.
    assert_eq!(
        store.admit_callback(123, 11, "query-b", task, interaction),
        Ok(Admission::Fresh)
    );
    assert_eq!(store.keys, HashSet::from([key(1)]));
    assert_eq!(store.task_keys.get(&task), Some(&key(1)));
    store.close(task).unwrap();
    drop(store);
    let path = dir.path().join("channel");
    let before = fs::read(path.join(JOURNAL)).unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.recover().unwrap(), vec![(task, Status::Closed)]);
    // A changed task/reference or retired task still cannot re-admit the query.
    assert_eq!(
        store.admit_callback(123, 90, "query-a", Uuid::from_u128(90), Uuid::from_u128(91)),
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        store.admit_callback(123, 10, "query-c", task, interaction),
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        store.admit_callback(123, 12, "query-c", task, interaction),
        Err(Failure::InvalidTransition)
    );
    assert_eq!(store.recover().unwrap(), vec![(task, Status::Closed)]);
    assert_eq!(fs::read(path.join(JOURNAL)).unwrap(), before);
}

#[test]
fn callback_rejects_invalid_ids_query_bot_and_task_state() {
    let (_dir, mut store) = setup();
    let task = Uuid::from_u128(1);
    let interaction = Uuid::from_u128(2);
    for query in [
        "".to_owned(),
        "q\nsecret".into(),
        "q\u{0085}".into(),
        "q".repeat(MAX_QUERY + 1),
    ] {
        assert_eq!(
            store.admit_callback(123, 0, &query, task, interaction),
            Err(Failure::InvalidInput)
        );
    }
    for (bot, update, task, interaction) in [
        (0, 1, task, interaction),
        (123, -1, task, interaction),
        (123, 1, Uuid::nil(), interaction),
        (123, 1, task, Uuid::nil()),
    ] {
        assert_eq!(
            store.admit_callback(bot, update, "q", task, interaction),
            Err(Failure::InvalidInput)
        );
    }
    assert_eq!(
        store.admit_callback(123, 10, "q", task, interaction),
        Err(Failure::InvalidTransition)
    );
    store.admit(key(1), 1, task).unwrap();
    assert_eq!(
        store.admit_callback(123, 10, "q", task, interaction),
        Err(Failure::InvalidTransition)
    );
    store.mark_submitting(task, Uuid::from_u128(3)).unwrap();
    assert_eq!(
        store.admit_callback(124, 10, "q", task, interaction),
        Err(Failure::InvalidTransition)
    );
    assert_eq!(
        store.admit_callback(123, 0, &"q".repeat(MAX_QUERY), task, interaction),
        Ok(Admission::Fresh)
    );
    store.close(task).unwrap();
    assert_eq!(
        store.admit_callback(123, 10, "q", task, interaction),
        Err(Failure::InvalidTransition)
    );
}

#[test]
fn callback_cannot_reinterpret_rejected_or_received_update_and_cannot_become_text() {
    let (dir, mut store) = setup();
    let (task, interaction) = submitting(&mut store);
    store.reject(123, 10, None).unwrap();
    drop(store);
    let mut store = Store::open(&dir.path().join("channel")).unwrap();
    for update in [1, 10] {
        assert_eq!(
            store.admit_callback(123, update, "query", task, interaction),
            Ok(Admission::Duplicate)
        );
    }
    assert_eq!(
        store.admit_callback(123, 11, "query", task, interaction),
        Ok(Admission::Fresh)
    );
    assert_eq!(
        store.admit(key(2), 11, Uuid::from_u128(4)),
        Ok(Admission::Duplicate)
    );
    let sequence = store.sequence;
    store.reject(123, 11, Some(key(2))).unwrap();
    assert_eq!(store.sequence, sequence);
    assert_eq!(
        store.admit(key(2), 12, Uuid::from_u128(4)),
        Ok(Admission::Fresh)
    );
}

#[test]
fn callback_metadata_shares_capacity_with_tasks_and_rejections_on_write_and_reopen() {
    let (dir, store) = setup();
    drop(store);
    let path = dir.path().join("channel");
    let task = Uuid::from_u128(1);
    let interaction = Uuid::from_u128(2);
    let mut records = vec![
        Record {
            version: 1,
            sequence: 1,
            event: Event::Received {
                key: key(1),
                update_id: 1,
                task,
            },
        },
        Record {
            version: 2,
            sequence: 2,
            event: Event::Submitting {
                task,
                client_id: Uuid::from_u128(3),
            },
        },
        Record {
            version: 2,
            sequence: 3,
            event: Event::Rejected {
                bot_id: 123,
                update_id: 2,
                message_key: None,
            },
        },
    ];
    for id in 3..MAX_TASKS {
        records.push(Record {
            version: 3,
            sequence: id as u64 + 1,
            event: Event::CallbackReceived {
                bot_id: 123,
                update_id: id as i64,
                query_digest: blake3::hash(id.to_string().as_bytes()).to_hex().to_string(),
                task,
                interaction,
            },
        });
    }
    let bytes = records
        .into_iter()
        .flat_map(fixture_line)
        .collect::<Vec<_>>();
    fs::write(path.join(JOURNAL), &bytes).unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.receipt_count(), MAX_TASKS - 1);
    assert_eq!(
        store.admit_callback(123, 5000, "last", task, interaction),
        Ok(Admission::Fresh)
    );
    for _ in 0..2 {
        assert_eq!(
            store.admit_callback(123, 5001, "excess", task, interaction),
            Err(Failure::Capacity)
        );
        assert_eq!(
            store.admit(key(2), 5001, Uuid::from_u128(4)),
            Err(Failure::Capacity)
        );
        assert_eq!(store.reject(123, 5001, None), Err(Failure::Capacity));
        assert_eq!(
            store.admit_callback(123, 5002, "last", task, interaction),
            Ok(Admission::Duplicate)
        );
        drop(store);
        store = Store::open(&path).unwrap();
    }
    let sequence = store.sequence + 1;
    drop(store);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path.join(JOURNAL))
        .unwrap();
    file.write_all(&fixture_line(Record {
        version: 3,
        sequence,
        event: Event::Rejected {
            bot_id: 123,
            update_id: 6000,
            message_key: None,
        },
    }))
    .unwrap();
    assert!(matches!(Store::open(&path), Err(Failure::Corrupt)));
}

#[test]
fn callback_requires_v3_and_mixed_journal_omits_query_body_action_native_id_and_token() {
    let (dir, store) = setup();
    drop(store);
    let path = dir.path().join("channel");
    let task = Uuid::from_u128(1);
    let interaction = Uuid::from_u128(2);
    let prefix = [
        Record {
            version: 1,
            sequence: 1,
            event: Event::Received {
                key: key(1),
                update_id: 1,
                task,
            },
        },
        Record {
            version: 2,
            sequence: 2,
            event: Event::Submitting {
                task,
                client_id: Uuid::from_u128(3),
            },
        },
    ]
    .into_iter()
    .flat_map(fixture_line)
    .collect::<Vec<_>>();
    for version in [1, 2, 3, 4, 5] {
        let mut bytes = prefix.clone();
        bytes.extend(fixture_line(Record {
            version,
            sequence: 3,
            event: Event::CallbackReceived {
                bot_id: 123,
                update_id: 2,
                query_digest: blake3::hash(b"query-only-in-memory").to_hex().to_string(),
                task,
                interaction,
            },
        }));
        fs::write(path.join(JOURNAL), bytes).unwrap();
        if matches!(version, 3 | 4) {
            let mut store = Store::open(&path).unwrap();
            store.save_token("123:synthetic_only").unwrap();
            assert_eq!(
                store.admit_callback(123, 3, "query-only-in-memory", task, interaction),
                Ok(Admission::Duplicate)
            );
            store.close(task).unwrap();
            let text = fs::read_to_string(path.join(JOURNAL)).unwrap();
            for secret in [
                "query-only-in-memory",
                "123:synthetic_only",
                "query_id",
                "body",
                "action",
                "native_id",
                "button",
            ] {
                // The UUID field is named interaction, so check complete JSON keys.
                let needle = if secret == "action" {
                    "\"action\""
                } else {
                    secret
                };
                assert!(!text.contains(needle));
            }
            let row: serde_json::Value =
                serde_json::from_str(text.lines().nth(2).unwrap()).unwrap();
            assert_eq!(row["record"]["event"].as_object().unwrap().len(), 6);
        } else {
            assert!(matches!(Store::open(&path), Err(Failure::Corrupt)));
        }
    }
}

#[test]
fn callback_fsync_failure_poisoning_preserves_uncertainty_and_reopen_never_replays() {
    let (dir, mut store) = setup();
    let (task, interaction) = submitting(&mut store);
    store.fail_next_sync = true;
    assert_eq!(
        store.admit_callback(123, 10, "uncertain-query", task, interaction),
        Err(Failure::Unavailable)
    );
    assert_eq!(
        store.admit_callback(123, 10, "uncertain-query", task, interaction),
        Err(Failure::Poisoned)
    );
    assert!(store.callback_updates.is_empty()); // no durable in-memory claim
    drop(store);
    // Model the possible full-write outcome of a failed fsync. A surviving row
    // becomes dedup metadata, never a callback or a native command to replay.
    let path = dir.path().join("channel");
    let before = fs::read(path.join(JOURNAL)).unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.recover().unwrap(), vec![(task, Status::Submitting)]);
    assert_eq!(
        store.admit_callback(123, 10, "uncertain-query", task, interaction),
        Ok(Admission::Duplicate)
    );
    assert_eq!(fs::read(path.join(JOURNAL)).unwrap(), before);
}

#[test]
fn callback_dedup_is_scoped_to_bot_and_replay_validates_digest_and_task_proof() {
    let (dir, mut store) = setup();
    let (task, interaction) = submitting(&mut store);
    let second_task = Uuid::from_u128(4);
    let second_key = DeliveryKey {
        bot_id: 124,
        ..key(2)
    };
    store.admit(second_key, 1, second_task).unwrap();
    store
        .mark_submitting(second_task, Uuid::from_u128(5))
        .unwrap();
    assert_eq!(
        store.admit_callback(123, 10, "query", task, interaction),
        Ok(Admission::Fresh)
    );
    assert_eq!(
        store.admit_callback(124, 10, "query", second_task, interaction),
        Ok(Admission::Fresh)
    );
    let sequence = store.sequence + 1;
    drop(store);
    let path = dir.path().join("channel");
    let valid = fs::read(path.join(JOURNAL)).unwrap();
    for (bot_id, update_id, query_digest, receipt_task, receipt_interaction) in [
        (123, 11, "not-a-digest".to_owned(), task, interaction),
        (123, 11, "A".repeat(64), task, interaction),
        (123, 11, "a".repeat(64), second_task, interaction),
        (123, 11, "a".repeat(64), task, Uuid::nil()),
        (123, 10, "a".repeat(64), task, interaction),
        (
            123,
            11,
            blake3::hash(b"query").to_hex().to_string(),
            task,
            interaction,
        ),
    ] {
        let mut corrupted = valid.clone();
        corrupted.extend(fixture_line(Record {
            version: 3,
            sequence,
            event: Event::CallbackReceived {
                bot_id,
                update_id,
                query_digest,
                task: receipt_task,
                interaction: receipt_interaction,
            },
        }));
        fs::write(path.join(JOURNAL), corrupted).unwrap();
        assert!(matches!(Store::open(&path), Err(Failure::Corrupt)));
    }
}

#[test]
fn question_reply_deduplicates_message_and_update_without_learning_conflicting_identities() {
    let (dir, mut store) = setup();
    let (task, interaction) = submitting(&mut store);
    assert_eq!(
        store.admit_question_reply(key(10), 10, task, interaction),
        Ok(Admission::Fresh)
    );
    let sequence = store.sequence;
    assert_eq!(
        store.admit_question_reply(key(10), 11, task, interaction),
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        store.admit_question_reply(key(11), 10, task, interaction),
        Ok(Admission::Duplicate)
    );
    assert_eq!(store.sequence, sequence);
    assert_eq!(
        store.admit_question_reply(key(11), 11, task, interaction),
        Ok(Admission::Fresh)
    );
    assert_eq!(store.keys, HashSet::from([key(1)]));
    assert_eq!(store.task_keys.get(&task), Some(&key(1)));
    assert_eq!(store.recover().unwrap(), vec![(task, Status::Submitting)]);
    store.close(task).unwrap();
    drop(store);
    let path = dir.path().join("channel");
    let before = fs::read(path.join(JOURNAL)).unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store.admit_question_reply(key(10), 90, Uuid::from_u128(90), Uuid::from_u128(91)),
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        store.admit_question_reply(key(12), 10, task, interaction),
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        store.admit_question_reply(key(12), 12, task, interaction),
        Err(Failure::InvalidTransition)
    );
    assert_eq!(store.recover().unwrap(), vec![(task, Status::Closed)]);
    assert_eq!(fs::read(path.join(JOURNAL)).unwrap(), before);
}

#[test]
fn question_reply_validates_original_bot_chat_task_status_and_identifiers() {
    let (_dir, mut store) = setup();
    let task = Uuid::from_u128(1);
    let interaction = Uuid::from_u128(2);
    for (key, update, task, interaction) in [
        (key(0), 10, task, interaction),
        (
            DeliveryKey {
                bot_id: 0,
                ..key(10)
            },
            10,
            task,
            interaction,
        ),
        (
            DeliveryKey {
                chat_id: 0,
                ..key(10)
            },
            10,
            task,
            interaction,
        ),
        (key(10), -1, task, interaction),
        (key(10), 10, Uuid::nil(), interaction),
        (key(10), 10, task, Uuid::nil()),
    ] {
        assert_eq!(
            store.admit_question_reply(key, update, task, interaction),
            Err(Failure::InvalidInput)
        );
    }
    assert_eq!(
        store.admit_question_reply(key(10), 10, task, interaction),
        Err(Failure::InvalidTransition)
    );
    store.admit(key(1), 1, task).unwrap();
    assert_eq!(
        store.admit_question_reply(key(10), 10, task, interaction),
        Err(Failure::InvalidTransition)
    );
    store.mark_submitting(task, Uuid::from_u128(3)).unwrap();
    for wrong in [
        DeliveryKey {
            bot_id: 124,
            ..key(10)
        },
        DeliveryKey {
            chat_id: 457,
            ..key(10)
        },
    ] {
        assert_eq!(
            store.admit_question_reply(wrong, 10, task, interaction),
            Err(Failure::InvalidTransition)
        );
    }
    assert_eq!(
        store.admit_question_reply(key(10), 0, task, interaction),
        Ok(Admission::Fresh)
    );
    store.close(task).unwrap();
    assert_eq!(
        store.admit_question_reply(key(11), 11, task, interaction),
        Err(Failure::InvalidTransition)
    );
}

#[test]
fn question_reply_cannot_reinterpret_any_prior_input_or_become_new_task_rejection_or_callback() {
    let (dir, mut store) = setup();
    let (task, interaction) = submitting(&mut store);
    store.reject(123, 20, Some(key(20))).unwrap();
    store
        .admit_callback(123, 30, "prior-query", task, interaction)
        .unwrap();
    drop(store);
    let path = dir.path().join("channel");
    let mut store = Store::open(&path).unwrap();
    for (message, update) in [(1, 100), (100, 1), (20, 200), (200, 20), (300, 30)] {
        assert_eq!(
            store.admit_question_reply(key(message), update, task, interaction),
            Ok(Admission::Duplicate)
        );
    }
    assert_eq!(
        store.admit_question_reply(key(100), 100, task, interaction),
        Ok(Admission::Fresh)
    );
    assert_eq!(
        store.admit_question_reply(key(200), 200, task, interaction),
        Ok(Admission::Fresh)
    );
    assert_eq!(
        store.admit_question_reply(key(300), 300, task, interaction),
        Ok(Admission::Fresh)
    );
    let sequence = store.sequence;
    assert_eq!(
        store.admit(key(100), 101, Uuid::from_u128(4)),
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        store.admit(key(101), 100, Uuid::from_u128(4)),
        Ok(Admission::Duplicate)
    );
    store.reject(123, 101, Some(key(100))).unwrap();
    store.reject(123, 100, Some(key(101))).unwrap();
    assert_eq!(
        store.admit_callback(123, 100, "new-query", task, interaction),
        Ok(Admission::Duplicate)
    );
    assert_eq!(store.sequence, sequence);
    assert_eq!(
        store.admit(key(101), 101, Uuid::from_u128(4)),
        Ok(Admission::Fresh)
    );
    assert_eq!(
        store.admit_callback(123, 102, "new-query", task, interaction),
        Ok(Admission::Fresh)
    );
}

#[test]
fn question_reply_only_v4_with_mixed_legacy_records_and_no_payload_fields() {
    let (dir, store) = setup();
    drop(store);
    let path = dir.path().join("channel");
    let task = Uuid::from_u128(1);
    let interaction = Uuid::from_u128(2);
    let prefix = [
        Record {
            version: 1,
            sequence: 1,
            event: Event::Received {
                key: key(1),
                update_id: 1,
                task,
            },
        },
        Record {
            version: 2,
            sequence: 2,
            event: Event::Submitting {
                task,
                client_id: Uuid::from_u128(3),
            },
        },
        Record {
            version: 3,
            sequence: 3,
            event: Event::CallbackReceived {
                bot_id: 123,
                update_id: 3,
                query_digest: blake3::hash(b"private-query").to_hex().to_string(),
                task,
                interaction,
            },
        },
    ]
    .into_iter()
    .flat_map(fixture_line)
    .collect::<Vec<_>>();
    for version in [1, 2, 3, 4, 5] {
        let mut bytes = prefix.clone();
        bytes.extend(fixture_line(Record {
            version,
            sequence: 4,
            event: Event::QuestionReplyReceived {
                key: key(4),
                update_id: 4,
                task,
                interaction,
            },
        }));
        fs::write(path.join(JOURNAL), bytes).unwrap();
        if version != 4 {
            assert!(matches!(Store::open(&path), Err(Failure::Corrupt)));
            continue;
        }
        let mut store = Store::open(&path).unwrap();
        store.save_token("123:synthetic_private_token").unwrap();
        assert_eq!(
            store.admit_question_reply(key(4), 5, task, interaction),
            Ok(Admission::Duplicate)
        );
        assert_eq!(
            store.admit_question_reply(key(5), 5, task, interaction),
            Ok(Admission::Fresh)
        );
        let text = fs::read_to_string(path.join(JOURNAL)).unwrap();
        for private in [
            "private-query",
            "synthetic_private_token",
            "\"text\"",
            "\"body\"",
            "\"answer\"",
            "\"answers\"",
            "\"reference\"",
            "\"query_id\"",
            "\"native_id\"",
        ] {
            assert!(!text.contains(private));
        }
        let row: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        let fields = row["record"]["event"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        assert_eq!(
            fields,
            HashSet::from(["kind", "key", "update_id", "task", "interaction"])
        );
        assert_eq!(row["record"]["version"], 4);
    }
}

#[test]
fn legacy_same_update_distinct_messages_remain_readable_with_original_chat_proofs() {
    let (dir, store) = setup();
    drop(store);
    let path = dir.path().join("channel");
    let first = Uuid::from_u128(1);
    let second = Uuid::from_u128(2);
    let second_key = DeliveryKey {
        chat_id: 789,
        ..key(2)
    };
    let interaction = Uuid::from_u128(3);
    let records = [
        Record {
            version: 1,
            sequence: 1,
            event: Event::Received {
                key: key(1),
                update_id: 1,
                task: first,
            },
        },
        Record {
            version: 2,
            sequence: 2,
            event: Event::Received {
                key: second_key,
                update_id: 1,
                task: second,
            },
        },
        Record {
            version: 2,
            sequence: 3,
            event: Event::Rejected {
                bot_id: 123,
                update_id: 1,
                message_key: None,
            },
        },
        Record {
            version: 2,
            sequence: 4,
            event: Event::Submitting {
                task: second,
                client_id: Uuid::from_u128(4),
            },
        },
        Record {
            version: 3,
            sequence: 5,
            event: Event::CallbackReceived {
                bot_id: 123,
                update_id: 5,
                query_digest: "a".repeat(64),
                task: second,
                interaction,
            },
        },
    ];
    fs::write(
        path.join(JOURNAL),
        records
            .into_iter()
            .flat_map(fixture_line)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.task_keys.get(&first), Some(&key(1)));
    assert_eq!(store.task_keys.get(&second), Some(&second_key));
    assert_eq!(
        store.admit_question_reply(key(6), 6, second, interaction),
        Err(Failure::InvalidTransition)
    );
    let reply = DeliveryKey {
        message_id: 6,
        ..second_key
    };
    assert_eq!(
        store.admit_question_reply(reply, 6, second, interaction),
        Ok(Admission::Fresh)
    );
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store.recover().unwrap(),
        vec![(first, Status::Received), (second, Status::Submitting)]
    );
    assert_eq!(
        store.admit_question_reply(reply, 7, second, interaction),
        Ok(Admission::Duplicate)
    );
}

#[test]
fn question_reply_capacity_is_shared_with_all_inputs_including_recovered_records() {
    let (dir, store) = setup();
    drop(store);
    let path = dir.path().join("channel");
    let task = Uuid::from_u128(1);
    let interaction = Uuid::from_u128(2);
    let mut records = vec![
        Record {
            version: 1,
            sequence: 1,
            event: Event::Received {
                key: key(1),
                update_id: 1,
                task,
            },
        },
        Record {
            version: 2,
            sequence: 2,
            event: Event::Submitting {
                task,
                client_id: Uuid::from_u128(3),
            },
        },
        Record {
            version: 2,
            sequence: 3,
            event: Event::Rejected {
                bot_id: 123,
                update_id: 2,
                message_key: None,
            },
        },
        Record {
            version: 3,
            sequence: 4,
            event: Event::CallbackReceived {
                bot_id: 123,
                update_id: 3,
                query_digest: "a".repeat(64),
                task,
                interaction,
            },
        },
    ];
    for id in 4..MAX_TASKS {
        records.push(Record {
            version: 4,
            sequence: id as u64 + 1,
            event: Event::QuestionReplyReceived {
                key: key(id as i64),
                update_id: id as i64,
                task,
                interaction,
            },
        });
    }
    fs::write(
        path.join(JOURNAL),
        records
            .into_iter()
            .flat_map(fixture_line)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.receipt_count(), MAX_TASKS - 1);
    assert_eq!(
        store.admit_question_reply(key(5000), 5000, task, interaction),
        Ok(Admission::Fresh)
    );
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.receipt_count(), MAX_TASKS);
    assert_eq!(
        store.admit_question_reply(key(5001), 5001, task, interaction),
        Err(Failure::Capacity)
    );
    assert_eq!(
        store.admit(key(5001), 5001, Uuid::from_u128(4)),
        Err(Failure::Capacity)
    );
    assert_eq!(store.reject(123, 5001, None), Err(Failure::Capacity));
    assert_eq!(
        store.admit_callback(123, 5001, "extra-query", task, interaction),
        Err(Failure::Capacity)
    );
    assert_eq!(
        store.admit_question_reply(key(5000), 5001, task, interaction),
        Ok(Admission::Duplicate)
    );
    let sequence = store.sequence + 1;
    drop(store);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path.join(JOURNAL))
        .unwrap();
    file.write_all(&fixture_line(Record {
        version: 4,
        sequence,
        event: Event::QuestionReplyReceived {
            key: key(6000),
            update_id: 6000,
            task,
            interaction,
        },
    }))
    .unwrap();
    assert!(matches!(Store::open(&path), Err(Failure::Corrupt)));
}

#[test]
fn question_reply_fsync_failure_poisoning_and_surviving_receipt_never_restore_answer() {
    let (dir, mut store) = setup();
    let (task, interaction) = submitting(&mut store);
    store.fail_next_sync = true;
    assert_eq!(
        store.admit_question_reply(key(10), 10, task, interaction),
        Err(Failure::Unavailable)
    );
    assert_eq!(
        store.admit_question_reply(key(10), 10, task, interaction),
        Err(Failure::Poisoned)
    );
    assert!(store.question_reply_updates.is_empty());
    drop(store);
    let path = dir.path().join("channel");
    let before = fs::read(path.join(JOURNAL)).unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.recover().unwrap(), vec![(task, Status::Submitting)]);
    assert_eq!(
        store.admit_question_reply(key(10), 11, task, interaction),
        Ok(Admission::Duplicate)
    );
    assert_eq!(fs::read(path.join(JOURNAL)).unwrap(), before);
}

#[test]
fn question_reply_recovery_rejects_invalid_ownership_duplicates_and_payload_fields() {
    let (dir, mut store) = setup();
    let (task, interaction) = submitting(&mut store);
    store
        .admit_question_reply(key(10), 10, task, interaction)
        .unwrap();
    let sequence = store.sequence + 1;
    drop(store);
    let path = dir.path().join("channel");
    let valid = fs::read(path.join(JOURNAL)).unwrap();
    for (key, update_id, reply_task, reply_interaction) in [
        (
            DeliveryKey {
                bot_id: 124,
                ..key(11)
            },
            11,
            task,
            interaction,
        ),
        (
            DeliveryKey {
                chat_id: 457,
                ..key(11)
            },
            11,
            task,
            interaction,
        ),
        (key(11), 11, Uuid::from_u128(99), interaction),
        (key(11), 11, task, Uuid::nil()),
        (key(11), 10, task, interaction),
        (key(10), 11, task, interaction),
        (key(1), 11, task, interaction),
        (key(11), 1, task, interaction),
    ] {
        let mut bytes = valid.clone();
        bytes.extend(fixture_line(Record {
            version: 4,
            sequence,
            event: Event::QuestionReplyReceived {
                key,
                update_id,
                task: reply_task,
                interaction: reply_interaction,
            },
        }));
        fs::write(path.join(JOURNAL), bytes).unwrap();
        assert!(matches!(Store::open(&path), Err(Failure::Corrupt)));
    }
    let row = fixture_line(Record {
        version: 4,
        sequence,
        event: Event::QuestionReplyReceived {
            key: key(11),
            update_id: 11,
            task,
            interaction,
        },
    });
    let mut row: serde_json::Value = serde_json::from_slice(&row).unwrap();
    row["record"]["event"]["answer"] = "synthetic-private-answer".into();
    assert!(serde_json::from_value::<Line>(row.clone()).is_err());
    let mut bytes = valid;
    bytes.extend(serde_json::to_vec(&row).unwrap());
    bytes.push(b'\n');
    fs::write(path.join(JOURNAL), bytes).unwrap();
    let Err(failure) = Store::open(&path) else {
        panic!("payload-bearing receipt must fail closed");
    };
    assert_eq!(failure, Failure::Corrupt);
    assert!(!format!("{failure:?} {failure}").contains("synthetic-private-answer"));
}
