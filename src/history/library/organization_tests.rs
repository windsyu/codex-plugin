use super::*;

fn row() -> Override {
    Override {
        source_identity: "synthetic-source".into(),
        identity_digest: "synthetic-identity".into(),
        disposition: "project".into(),
        project_id: Some("p_synthetic".into()),
        project_path: Some("/synthetic/project".into()),
    }
}
#[test]
fn organization_absence_is_read_only_and_first_save_is_shared() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("absent-data");
    let first = Store::new(data.clone());
    assert!(!data.exists());
    assert_eq!(first.snapshot().unwrap().revision, "absent");
    assert!(!data.exists());
    let saved = first.save("absent", "entry", |_| Ok(Some(row()))).unwrap();
    assert_ne!(saved.revision, "absent");
    assert_eq!(saved.overrides["entry"], row());
    let second = Store::new(data);
    assert_eq!(second.snapshot().unwrap(), saved);
    assert_eq!(
        second.save("absent", "entry", |_| Ok(None)).unwrap_err(),
        "organization_changed"
    );
}

#[test]
fn organization_same_revision_and_absent_cas_have_one_winner() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    for initial in [true, false] {
        let baseline = if initial {
            "absent".into()
        } else {
            Store::new(data.clone()).snapshot().unwrap().revision
        };
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = (0..2)
            .map(|i| {
                let store = Store::new(data.clone());
                let barrier = barrier.clone();
                let baseline = baseline.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.save(&baseline, &format!("entry-{i}"), |_| Ok(Some(row())))
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err("organization_changed")))
                .count(),
            1
        );
    }
}

#[test]
fn organization_initial_validation_failure_does_not_publish_state() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path().join("data"));
    assert_eq!(
        store
            .save("absent", "entry", |_| Err("validation_failed"))
            .unwrap_err(),
        "validation_failed"
    );
    assert_eq!(store.snapshot().unwrap().revision, "absent");
    assert!(!temp.path().join("data/organization/state.sqlite").exists());
    assert!(
        std::fs::read_dir(temp.path().join("data/organization"))
            .unwrap()
            .any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("pending-"))
    );
    assert!(store.save("absent", "entry", |_| Ok(Some(row()))).is_ok());
    let invalid = temp.path().join("invalid");
    let dir = Directory::root(&invalid)
        .unwrap()
        .dir("organization", true)
        .unwrap();
    dir.open("state.sqlite", true).unwrap();
    let invalid = Store::new(invalid);
    assert_eq!(invalid.snapshot().unwrap_err(), "organization_unavailable");
    assert_eq!(
        invalid
            .save("absent", "entry", |_| Ok(Some(row())))
            .unwrap_err(),
        "organization_unavailable"
    );
}

#[test]
fn organization_committed_state_is_not_cached_or_disposable() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let first = Store::new(data.clone());
    let saved = first.save("absent", "entry", |_| Ok(Some(row()))).unwrap();
    let second = Store::new(data.clone());
    assert_eq!(second.snapshot().unwrap(), saved);
    let cleared = first.save(&saved.revision, "entry", |_| Ok(None)).unwrap();
    assert_eq!(second.snapshot().unwrap(), cleared);
    assert!(cleared.overrides.is_empty());
    std::fs::remove_file(data.join("organization/state.sqlite")).unwrap();
    assert_eq!(first.snapshot().unwrap_err(), "organization_unavailable");
    assert_eq!(
        second
            .save(&cleared.revision, "entry", |_| Ok(Some(row())))
            .unwrap_err(),
        "organization_unavailable"
    );
    assert!(!data.join("organization/state.sqlite").exists());
}

#[test]
fn organization_rejects_main_and_sidecar_links_and_wide_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for name in [
        "state.sqlite",
        "state.sqlite-journal",
        "state.sqlite-wal",
        "state.sqlite-shm",
        "bootstrap.lock",
    ] {
        for hard in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let data = temp.path().join("data");
            let store = Store::new(data.clone());
            let saved = store.save("absent", "entry", |_| Ok(Some(row()))).unwrap();
            let external = temp.path().join("external");
            std::fs::write(&external, "unchanged").unwrap();
            std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o600)).unwrap();
            let path = data.join("organization").join(name);
            if path.exists() {
                std::fs::remove_file(&path).unwrap();
            }
            if hard {
                std::fs::hard_link(&external, &path).unwrap();
            } else {
                symlink(&external, &path).unwrap();
            }
            assert_eq!(
                store
                    .save(&saved.revision, "entry", |_| Ok(None))
                    .unwrap_err(),
                "unsafe_organization",
                "{name}/{hard}"
            );
            if name != "bootstrap.lock" {
                assert!(store.snapshot().is_err());
            }
            assert_eq!(std::fs::read_to_string(external).unwrap(), "unchanged");
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = Store::new(data.clone());
    store.save("absent", "entry", |_| Ok(Some(row()))).unwrap();
    std::fs::set_permissions(
        data.join("organization/state.sqlite"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert_eq!(store.snapshot().unwrap_err(), "unsafe_organization");
}

#[test]
fn organization_directory_links_permissions_and_bad_rows_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for root_target in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let store = Store::new(data.clone());
        store.save("absent", "entry", |_| Ok(Some(row()))).unwrap();
        let path = if root_target {
            data.clone()
        } else {
            data.join("organization")
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(store.snapshot().unwrap_err(), "unsafe_organization");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let relocated = temp.path().join("relocated");
        std::fs::rename(&path, &relocated).unwrap();
        symlink(&relocated, &path).unwrap();
        assert_eq!(store.snapshot().unwrap_err(), "unsafe_organization");
    }
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path().join("data"));
    let saved = store.save("absent", "entry", |_| Ok(Some(row()))).unwrap();
    for invalid in [
        Override {
            disposition: "future".into(),
            ..row()
        },
        Override {
            project_path: Some("relative".into()),
            ..row()
        },
        Override {
            disposition: "unassigned".into(),
            ..row()
        },
        Override {
            project_id: None,
            ..row()
        },
    ] {
        assert_eq!(
            store
                .save(&saved.revision, "entry", |_| Ok(Some(invalid)))
                .unwrap_err(),
            "organization_unavailable"
        );
        assert_eq!(store.snapshot().unwrap(), saved);
    }
}

#[test]
fn organization_replacement_version_corruption_and_busy_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = Store::new(data.clone());
    let saved = store.save("absent", "entry", |_| Ok(Some(row()))).unwrap();
    let path = data.join("organization/state.sqlite");
    let replacement = data.join("organization/replacement.sqlite");
    std::fs::copy(&path, &replacement).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert_eq!(store.snapshot().unwrap_err(), "organization_unavailable");
    let fresh = Store::new(data.clone());
    let observed = fresh.snapshot().unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let before = Instant::now();
    assert_eq!(
        fresh
            .save(&observed.revision, "entry", |_| Ok(None))
            .unwrap_err(),
        "organization_unavailable"
    );
    assert!(before.elapsed() < Duration::from_secs(2));
    db.execute_batch("ROLLBACK; PRAGMA user_version=99")
        .unwrap();
    assert_eq!(fresh.snapshot().unwrap_err(), "organization_unavailable");
    db.execute_batch("PRAGMA user_version=1; UPDATE organization_meta SET store_id='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb'").unwrap();
    assert_eq!(fresh.snapshot().unwrap_err(), "organization_unavailable");
    drop(db);
    std::fs::write(&path, "synthetic corrupt state").unwrap();
    let contents = std::fs::read(&path).unwrap();
    assert_eq!(
        Store::new(data).snapshot().unwrap_err(),
        "organization_unavailable"
    );
    assert_eq!(std::fs::read(path).unwrap(), contents);
    assert!(!saved.overrides.is_empty());
}

#[test]
fn organization_real_sqlite_full_rolls_back_prior_revision() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path().join("data"));
    let saved = store.save("absent", "entry", |_| Ok(Some(row()))).unwrap();
    *store.fault.lock().unwrap() = Some(Fault::Full);
    assert_eq!(
        store
            .save(&saved.revision, "other", |_| Ok(Some(row())))
            .unwrap_err(),
        "organization_unavailable"
    );
    *store.fault.lock().unwrap() = None;
    assert_eq!(store.snapshot().unwrap(), saved);
}

#[test]
fn organization_row_and_byte_budgets_do_not_publish_partial_updates() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = Store::new(data.clone());
    let saved = store.save("absent", "entry", |_| Ok(Some(row()))).unwrap();
    let mut db = Connection::open(data.join("organization/state.sqlite")).unwrap();
    let tx = db.transaction().unwrap();
    for i in 1..ROWS {
        tx.execute(
            "INSERT INTO project_overrides VALUES(?1,'source','identity','unassigned',NULL,NULL)",
            [format!("entry-{i}")],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let full = store.snapshot().unwrap();
    assert_eq!(full.overrides.len(), ROWS);
    assert_eq!(
        store
            .save(&full.revision, "one-too-many", |_| Ok(Some(row())))
            .unwrap_err(),
        "organization_budget_exceeded"
    );
    assert_eq!(store.snapshot().unwrap(), full);
    let oversized = Override {
        source_identity: "x".repeat(BYTES + 1),
        ..row()
    };
    assert_eq!(
        store
            .save(&full.revision, "entry", |_| Ok(Some(oversized)))
            .unwrap_err(),
        "organization_budget_exceeded"
    );
    assert_eq!(store.snapshot().unwrap(), full);
    let encoded = BTreeMap::from_iter((0..600).map(|i| {
        (
            format!("entry-{i}"),
            Override {
                source_identity: "\n".repeat(FIELD),
                identity_digest: "\n".repeat(FIELD),
                ..row()
            },
        )
    }));
    assert_eq!(budget(&encoded), Err("organization_budget_exceeded"));
    assert_eq!(saved.revision, full.revision);
}

#[test]
fn organization_crash_child() {
    let Some(data) = std::env::var_os("ORGANIZATION_CRASH_TEST_DATA") else {
        return;
    };
    let store = Store::new(PathBuf::from(data));
    let snapshot = store.snapshot().unwrap();
    *store.fault.lock().unwrap() = Some(
        if std::env::var("ORGANIZATION_CRASH_TEST_PHASE").unwrap() == "before" {
            Fault::BeforeCommitCrash
        } else {
            Fault::AfterCommitCrash
        },
    );
    store
        .save(&snapshot.revision, "crash-entry", |_| Ok(Some(row())))
        .unwrap();
    panic!("crash hook did not terminate child");
}

#[test]
fn organization_process_crash_and_lost_ack_preserve_transaction_boundaries() {
    for (phase, status) in [("before", 55), ("after", 56)] {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let store = Store::new(data.clone());
        let saved = store.save("absent", "entry", |_| Ok(Some(row()))).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "history::library::organization::tests::organization_crash_child",
                "--nocapture",
            ])
            .env("ORGANIZATION_CRASH_TEST_DATA", &data)
            .env("ORGANIZATION_CRASH_TEST_PHASE", phase)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(child.code(), Some(status));
        if phase == "before" {
            // Production snapshot performs controlled native recovery itself.
            assert_eq!(store.snapshot().unwrap(), saved);
        } else {
            let committed = Store::new(data).snapshot().unwrap();
            assert_ne!(committed.revision, saved.revision);
            assert!(committed.overrides.contains_key("crash-entry"));
            assert_eq!(
                store
                    .save(&saved.revision, "crash-entry", |_| Ok(Some(row())))
                    .unwrap_err(),
                "organization_changed"
            );
        }
    }
}

#[test]
fn organization_first_creation_crash_leaves_only_pending_but_committed_ack_loss_is_readable() {
    for (phase, status) in [("before", 55), ("after", 56)] {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "history::library::organization::tests::organization_crash_child",
                "--nocapture",
            ])
            .env("ORGANIZATION_CRASH_TEST_DATA", &data)
            .env("ORGANIZATION_CRASH_TEST_PHASE", phase)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(child.code(), Some(status));
        let store = Store::new(data.clone());
        if phase == "before" {
            assert_eq!(store.snapshot().unwrap().revision, "absent");
            assert!(!data.join("organization/state.sqlite").exists());
            assert!(store.save("absent", "entry", |_| Ok(Some(row()))).is_ok());
        } else {
            let saved = store.snapshot().unwrap();
            assert!(saved.overrides.contains_key("crash-entry"));
            assert_eq!(
                store.save("absent", "entry", |_| Ok(None)).unwrap_err(),
                "organization_changed"
            );
        }
    }
}
