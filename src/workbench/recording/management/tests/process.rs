use super::*;

// Executed only by the parent below, with a temporary synthetic data root.
#[test]
#[ignore = "subprocess helper for private synthetic crash/lock fixtures"]
fn cleanup_process_helper() {
    let base = PathBuf::from(std::env::var_os("R31_TEST_ROOT").unwrap());
    let cwd = base.join("project");
    let prepared = Prepared::load(
        &base.join("home"),
        &cwd,
        &crate::workbench::paths::WorkbenchPaths::from_user_home(&base).unwrap(),
        None,
        Overrides::default(),
    )
    .unwrap();
    let root = prepared.data_dir.clone();
    let config = ConfigService::start(prepared).unwrap();
    let identity = Directory::root(&root).unwrap().identity().unwrap();
    let workspace = blake3::hash(cwd.as_os_str().as_encoded_bytes())
        .to_hex()
        .to_string();
    let id = Uuid::parse_str(&std::env::var("R31_TEST_JOB").unwrap()).unwrap();
    let attempt = cleanup::Task::resume(
        &root,
        &workspace,
        Uuid::new_v4(),
        config.handle(),
        id,
        identity,
    );
    if std::env::var("R31_TEST_STAGE").unwrap() == "locked" {
        assert!(matches!(attempt, Err(Error::Busy)));
        std::process::exit(85);
    }
    let mut task = attempt.unwrap();
    task.crash = Some("after_rename");
    for _ in 0..1000 {
        match task.step() {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                // No Rust destructors, service shutdown or task checkpoint.
                std::process::exit(86);
            }
            Ok(false) => {}
            _ => panic!("expected interruption immediately after quarantine rename"),
        }
    }
    panic!("crash checkpoint not reached");
}

#[test]
fn separate_process_lock_abrupt_exit_policy_pause_and_startup_recovery_preserve_scope() {
    let f = Fixture::new();
    f.enable_manual(true);
    let (id, writer) = f.run(false, true);
    drop(writer);
    let (other, writer) = f.run(true, true);
    drop(writer);
    let mut e = f.engine();
    let p = preview(&mut e, PreviewMode::Manual(vec![id]));
    let operation = Uuid::new_v4();
    let task = cleanup::Task::create(
        &f.root,
        &f.workspace,
        f.current,
        f.config.handle(),
        &p,
        p.config_revision.as_deref().unwrap(),
        operation,
    )
    .unwrap();
    let child = |stage: &str| {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "workbench::recording::management::tests::process::cleanup_process_helper",
                "--ignored",
            ])
            .env("R31_TEST_ROOT", f._tmp.path())
            .env("R31_TEST_JOB", operation.to_string())
            .env("R31_TEST_STAGE", stage)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap()
            .code()
    };
    assert_eq!(child("locked"), Some(85));
    drop(task);
    drop(e);
    assert_eq!(child("after_rename"), Some(86));
    assert!(!f.root.join("runs").join(id.to_string()).exists());
    let quarantined = f
        .root
        .join("cleanup/trash")
        .join(operation.to_string())
        .join(id.to_string());
    assert!(quarantined.join("meta.json").exists());
    f.enable_manual(false);
    let mut restarted = f.engine();
    finish_scan(&mut restarted);
    assert_eq!(
        restarted.query(Query::Job { id: operation }).unwrap()["result"]["status"],
        "paused"
    );
    std::fs::write(f._tmp.path().join(".codex-web/config/config.json"), b"{").unwrap();
    restarted.retention.policy_at = Instant::now();
    restarted.tick();
    assert_eq!(
        restarted.query(Query::Job { id: operation }).unwrap()["result"]["pauseReason"],
        "config_unavailable"
    );
    assert!(quarantined.exists());
    f.enable_manual(true);
    restarted.retention.policy_at = Instant::now();
    for _ in 0..10000 {
        restarted.tick();
        if restarted.query(Query::Job { id: operation }).unwrap()["result"]["status"] == "complete"
        {
            break;
        }
    }
    assert_eq!(
        restarted.query(Query::Job { id: operation }).unwrap()["result"]["items"][0]["state"],
        "deleted"
    );
    assert!(!quarantined.exists());
    assert!(f.root.join("runs").join(other.to_string()).exists());
}

#[test]
fn job_pages_are_bounded_repeatable_and_invalidated_by_external_changes() {
    let f = Fixture::new();
    f.enable_manual(true);
    let (id, w) = f.run(false, true);
    drop(w);
    let mut e = f.engine();
    let p = preview(&mut e, PreviewMode::Manual(vec![id]));
    let preview_observed_at = Instant::now();
    let preview_observed_wall = chrono::Utc::now();
    let create_cancelled = || {
        let operation = Uuid::new_v4();
        let policy_before = f
            .config
            .handle()
            .history_policy()
            .map(|(_, revision)| revision);
        let mut task = cleanup::Task::create(
            &f.root,
            &f.workspace,
            f.current,
            f.config.handle(),
            &p,
            p.config_revision.as_deref().unwrap(),
            operation,
        )
        .unwrap_or_else(|error| {
            // Diagnose the fail-closed branch without printing fixture paths,
            // policy bodies or history payloads, and without retrying creation.
            let now = chrono::Utc::now();
            let policy_after = f.config.handle().history_policy().map(|(_, revision)| revision);
            let root_matches = Directory::root(&f.root)
                .and_then(|root| root.identity())
                .map(|identity| Some(identity) == p.root_identity);
            panic!(
                "synthetic cancelled-job creation failed: error={error:?}, monotonic_elapsed={:?}, wall_elapsed_ms={}, expires_at={}, now={}, preview_revision={:?}, policy_revision_before={policy_before:?}, policy_revision_after={policy_after:?}, preview_root_present={}, root_matches={root_matches:?}, operation_job_exists={:?}, operation_trash_exists={:?}",
                preview_observed_at.elapsed(),
                now.signed_duration_since(preview_observed_wall).num_milliseconds(),
                p.expires_at,
                now.to_rfc3339(),
                p.config_revision,
                p.root_identity.is_some(),
                f.root.join("cleanup/jobs").join(format!("{operation}.json")).try_exists(),
                f.root.join("cleanup/trash").join(operation.to_string()).try_exists(),
            );
        });
        task.cancel().unwrap();
    };
    for _ in 0..25 {
        create_cancelled();
    }
    let mut cursor = jobs::Cursor::new(Directory::root(&f.root).unwrap()).unwrap();
    let first = cursor.page(&f.workspace, None).unwrap();
    assert_eq!(first["jobs"].as_array().unwrap().len(), 20);
    assert_eq!(cursor.page(&f.workspace, None).unwrap(), first);
    let next = first["nextCursor"].as_str().unwrap();
    let second = cursor.page(&f.workspace, Some(next)).unwrap();
    assert_eq!(second["jobs"].as_array().unwrap().len(), 5);
    assert!(second["nextCursor"].is_null());
    let ids = first["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["jobs"].as_array().unwrap())
        .map(|j| j["jobId"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), 25);
    create_cancelled();
    assert_eq!(
        cursor.page(&f.workspace, Some(next)).unwrap_err(),
        Error::Stale
    );
    let mut cursor = jobs::Cursor::new(Directory::root(&f.root).unwrap()).unwrap();
    assert!(
        cursor.page(&"b".repeat(64), None).unwrap()["jobs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
