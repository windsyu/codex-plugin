use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

pub(super) fn fixture() -> (tempfile::TempDir, Prepared) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    let config = Prepared::load(
        &home,
        &cwd,
        &crate::workbench::paths::WorkbenchPaths::from_user_home(home.parent().unwrap()).unwrap(),
        None,
        Overrides::default(),
    )
    .unwrap();
    (temp, config)
}
#[test]
fn defaults_match_documented_json_and_partial_fields_keep_deletion_disabled() {
    let example = Config::parse(include_bytes!(
        "../../../docs/configuration/workbench.config.example.json"
    ))
    .unwrap();
    assert_eq!(example, Config::default());
    for json in [
        r#"{"schemaVersion":1}"#,
        r#"{"schemaVersion":1,"launch":{"openBrowser":false},"history":{"cleanup":{"retention":{}}}}"#,
    ] {
        let config = Config::parse(json.as_bytes()).unwrap();
        assert!(!config.history.cleanup.enabled);
        assert!(!config.history.cleanup.retention.enabled);
        assert_eq!(config.history.cleanup.retention.days, 90);
    }
}
#[test]
fn invalid_and_duplicate_fields_are_rejected_without_echoing_values() {
    for json in [
        r#"{}"#,
        r#"{"schemaVersion":2}"#,
        r#"{"schemaVersion":1,"schemaVersion":1}"#,
        r#"{"schemaVersion":1,"launch":{"profile":"SECRET","profile":"other"}}"#,
        r#"{"schemaVersion":1,"SECRET":true}"#,
        r#"{"schemaVersion":1,"launch":{"openBrowser":"SECRET"}}"#,
        r#"{"schemaVersion":1,"history":{"cleanup":{"enabled":true,"enabled":false}}}"#,
        r#"{"schemaVersion":1,"launch":{"codexBin":"SECRET"}}"#,
        r#"{"schemaVersion":1,"launch":{"profile":"SECRET/../other"}}"#,
        r#"{"schemaVersion":1,"history":{"cleanup":{"retention":{"days":0}}}}"#,
        r#"{"schemaVersion":1,"history":{"cleanup":{"retention":{"enabled":true}}}}"#,
        r#"{"schemaVersion":1} {}"#,
    ] {
        let error = Config::parse(json.as_bytes()).unwrap_err();
        assert!(!format!("{error:?}").contains("SECRET"));
    }
    assert_eq!(
        Config::parse(br#"{"schemaVersion":1,"launch":{"openBrowser":12}}"#)
            .unwrap_err()
            .field,
        "launch.openBrowser"
    );
    assert_eq!(
        Config::parse(&vec![b' '; LIMIT + 1]).unwrap_err().code,
        "config_too_large"
    );
}
#[test]
fn initial_files_are_private_and_existing_config_is_never_reset() {
    let (_temp, prepared) = fixture();
    assert_eq!(prepared.directory.path, prepared.paths.config_dir());
    assert_eq!(prepared.data_dir, prepared.paths.history_dir());
    assert!(!prepared.home.join("workbench").exists());
    assert!(!prepared.home.join("workbench-data-v1").exists());
    assert_eq!(
        std::fs::metadata(&prepared.paths.root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for name in ["config.json", "config.schema.json", "config.lock"] {
        assert_eq!(
            std::fs::metadata(prepared.directory.path.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        std::fs::metadata(&prepared.directory.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let minimal = br#"{"schemaVersion":1,"launch":{"openBrowser":false}}"#;
    std::fs::write(prepared.directory.path.join("config.json"), minimal).unwrap();
    let again = Prepared::load(
        &prepared.home,
        &prepared.cwd,
        &prepared.paths,
        None,
        Overrides::default(),
    )
    .unwrap();
    assert!(!again.effective.launch.open_browser);
    assert_eq!(
        prepared.directory.read("config.json", LIMIT).unwrap(),
        minimal
    );
}

#[test]
fn new_defaults_preserve_legacy_files_and_explicit_paths_can_still_use_them() {
    let temp = tempfile::tempdir().unwrap();
    let user = temp.path().join("user");
    let home = temp.path().join("separate-native-home");
    let cwd = temp.path().join("project");
    for dir in [&user, &home, &cwd] {
        std::fs::create_dir(dir).unwrap();
    }
    let paths = WorkbenchPaths::from_user_home(&user).unwrap();
    let legacy_parent = Directory::root(&home.join("workbench")).unwrap();
    let legacy_config = legacy_parent.dir("config", true).unwrap();
    let legacy_history = Directory::root(&home.join("workbench-data-v1")).unwrap();
    let native = b"synthetic native config must stay unchanged";
    let log = b"synthetic existing observation log";
    std::fs::write(home.join("config.toml"), native).unwrap();
    legacy_history.atomic("keep.jsonl", log).unwrap();
    let mut old = Config::default();
    old.launch.open_browser = false;
    old.history.cleanup.enabled = true;
    let old_bytes = encoded(&old).unwrap();
    legacy_config.atomic("config.json", &old_bytes).unwrap();

    let prepared = Prepared::load(&home, &cwd, &paths, None, Overrides::default()).unwrap();
    assert_eq!(
        prepared.effective,
        Config::default(),
        "old settings are not silently selected"
    );
    assert_eq!(
        prepared.snapshot().config_path,
        paths.config_dir().join("config.json")
    );
    assert_eq!(prepared.data_dir, paths.history_dir());
    let new_bytes = prepared.directory.read("config.json", LIMIT).unwrap();
    let selected = Prepared::load(
        &home,
        &cwd,
        &paths,
        Some(&legacy_config.path),
        Overrides {
            data_dir: Some(legacy_history.path.clone()),
            ..Overrides::default()
        },
    )
    .unwrap();
    assert!(!selected.effective.launch.open_browser);
    assert_eq!(
        selected.data_dir,
        legacy_history.path.canonicalize().unwrap()
    );
    assert_eq!(legacy_config.read("config.json", LIMIT).unwrap(), old_bytes);
    assert_eq!(legacy_history.read("keep.jsonl", LIMIT).unwrap(), log);
    assert_eq!(std::fs::read(home.join("config.toml")).unwrap(), native);
    assert_eq!(
        prepared.directory.read("config.json", LIMIT).unwrap(),
        new_bytes
    );
}

#[test]
fn default_product_root_rejects_symlinks_without_writing_their_destination() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("native");
    let cwd = temp.path().join("project");
    let target = temp.path().join("outside");
    for dir in [&home, &cwd, &target] {
        std::fs::create_dir(dir).unwrap();
    }
    let paths = WorkbenchPaths::from_user_home(temp.path()).unwrap();
    symlink(&target, &paths.root).unwrap();
    assert!(Prepared::load(&home, &cwd, &paths, None, Overrides::default()).is_err());
    let custom = temp.path().join("preferences");
    assert!(Prepared::load(&home, &cwd, &paths, Some(&custom), Overrides::default()).is_err());
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
}
#[test]
fn invocation_overrides_are_not_persisted_and_saved_changes_require_restart() {
    let (_temp, prepared) = fixture();
    let prepared = Prepared::load(
        &prepared.home,
        &prepared.cwd,
        &prepared.paths,
        None,
        Overrides {
            open_browser: Some(false),
            profile: Some("one_off".into()),
            ..Overrides::default()
        },
    )
    .unwrap();
    let before = prepared.snapshot();
    assert!(!before.effective.launch.open_browser);
    assert!(before.saved.as_ref().unwrap().launch.open_browser);
    assert_eq!(
        before.cli_overrides,
        vec!["launch.openBrowser", "launch.profile"]
    );
    let mut config = before.saved.unwrap();
    config.launch.open_browser = false;
    config.launch.profile = Some("future_profile".into());
    config.storage.data_dir = Some(prepared.cwd.join("private-history"));
    let saved = prepared
        .save(
            before.revision.as_deref().unwrap(),
            &config,
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap();
    assert_eq!(saved.effective.launch.profile.as_deref(), Some("one_off"));
    assert_eq!(saved.restart_required, vec!["storage.dataDir"]);
    assert_eq!(saved.effective_data_dir, prepared.paths.history_dir());
    assert!(!config.storage.data_dir.unwrap().exists());
}
#[test]
fn stale_revisions_and_backup_failures_do_not_overwrite_config() {
    let (temp, prepared) = fixture();
    let before = prepared.snapshot();
    let mut changed = Config::default();
    changed.launch.open_browser = false;
    prepared
        .save(
            before.revision.as_deref().unwrap(),
            &changed,
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap();
    assert_eq!(
        Config::parse(
            &prepared
                .directory
                .read("config.previous.json", LIMIT)
                .unwrap()
        )
        .unwrap(),
        Config::default()
    );
    assert_eq!(
        prepared
            .save(
                before.revision.as_deref().unwrap(),
                &Config::default(),
                Instant::now() + Duration::from_secs(3)
            )
            .unwrap_err()
            .code,
        "config_changed"
    );
    let before = prepared.snapshot();
    let outside = temp.path().join("outside");
    std::fs::write(&outside, b"keep").unwrap();
    prepared.directory.remove("config.previous.json").unwrap();
    symlink(
        &outside,
        prepared.directory.path.join("config.previous.json"),
    )
    .unwrap();
    assert!(
        prepared
            .save(
                before.revision.as_deref().unwrap(),
                &Config::default(),
                Instant::now() + Duration::from_secs(3)
            )
            .is_err()
    );
    assert_eq!(prepared.read().unwrap().0, changed);
    assert_eq!(std::fs::read(outside).unwrap(), b"keep");
}
#[test]
fn corrupt_file_symlink_hardlink_and_replaced_root_are_readable_errors() {
    let (_temp, prepared) = fixture();
    let path = prepared.directory.path.join("config.json");
    std::fs::write(&path, b"broken SECRET").unwrap();
    let snapshot = prepared.snapshot();
    assert!(snapshot.saved.is_none());
    assert!(!snapshot.errors.is_empty());
    assert!(!snapshot.effective.history.cleanup.enabled);
    assert!(!serde_json::to_string(&snapshot).unwrap().contains("SECRET"));
    assert!(
        Prepared::load(
            &prepared.home,
            &prepared.cwd,
            &prepared.paths,
            None,
            Overrides::default()
        )
        .is_err()
    );
    let outside = prepared.home.join("other.json");
    std::fs::write(&outside, encoded(&Config::default()).unwrap()).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::remove_file(&path).unwrap();
    symlink(&outside, &path).unwrap();
    assert!(!prepared.snapshot().errors.is_empty());
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&outside, &path).unwrap();
    assert!(!prepared.snapshot().errors.is_empty());
    std::fs::rename(&prepared.directory.path, prepared.home.join("moved-config")).unwrap();
    assert!(!prepared.snapshot().errors.is_empty());
}
#[test]
fn cleanup_and_retention_require_their_explicit_saved_switches() {
    let (_temp, prepared) = fixture();
    let mut config = Config::default();
    config.history.cleanup.enabled = true;
    assert!(prepared.validate(&config).is_ok());
    config.history.cleanup.retention.enabled = true;
    assert!(prepared.validate(&config).is_ok());
    config.history.cleanup.enabled = false;
    assert!(prepared.validate(&config).is_err());
}
#[test]
fn protected_data_paths_and_overlapping_config_roots_are_rejected() {
    let (_temp, prepared) = fixture();
    for path in [
        &prepared.home,
        &prepared.cwd,
        &prepared.directory.path,
        &prepared.home.join("sessions"),
        &prepared.home.join("archived_sessions/subdir"),
    ] {
        let mut config = Config::default();
        config.storage.data_dir = Some(path.to_path_buf());
        assert_eq!(
            prepared.validate(&config).unwrap_err().code,
            "unsafe_data_directory"
        );
    }
    assert!(
        Prepared::load(
            &prepared.home,
            &prepared.cwd,
            &prepared.paths,
            Some(&prepared.home),
            Overrides::default()
        )
        .is_err()
    );
    assert!(
        Prepared::load(
            &prepared.home,
            &prepared.cwd,
            &prepared.paths,
            None,
            Overrides {
                data_dir: Some(prepared.directory.path.clone()),
                ..Overrides::default()
            }
        )
        .is_err()
    );
}
#[test]
fn concurrent_writer_lock_and_expired_requests_do_not_change_settings() {
    let (_temp, prepared) = fixture();
    let snapshot = prepared.snapshot();
    let mut config = Config::default();
    config.launch.open_browser = false;
    let guard = lock(&prepared.directory).unwrap();
    assert_eq!(
        prepared
            .save(
                snapshot.revision.as_deref().unwrap(),
                &config,
                Instant::now() + Duration::from_secs(3)
            )
            .unwrap_err()
            .code,
        "config_busy"
    );
    drop(guard);
    assert_eq!(
        prepared
            .save(
                snapshot.revision.as_deref().unwrap(),
                &config,
                Instant::now() - Duration::from_secs(1)
            )
            .unwrap_err()
            .code,
        "config_busy"
    );
    assert_eq!(prepared.read().unwrap().0, Config::default());
}
#[tokio::test]
async fn worker_reads_external_edits_and_fails_closed_without_changing_launch_values() {
    let (_temp, prepared) = fixture();
    let path = prepared.directory.path.join("config.json");
    let service = ConfigService::start(prepared).unwrap();
    let handle = service.handle();
    let mut config = Config::default();
    config.launch.open_browser = false;
    std::fs::write(&path, encoded(&config).unwrap()).unwrap();
    let loaded = handle.read().await.unwrap();
    assert_eq!(loaded.saved, Some(config));
    assert!(loaded.effective.launch.open_browser);
    assert_eq!(loaded.restart_required, vec!["launch.openBrowser"]);
    std::fs::write(&path, b"{").unwrap();
    let loaded = handle.read().await.unwrap();
    assert!(loaded.saved.is_none());
    assert!(!loaded.effective.history.cleanup.enabled);
    assert!(loaded.effective.launch.open_browser);
}

#[test]
fn repeated_file_notifications_leave_capacity_for_configuration_requests() {
    let (sender, receiver) = mpsc::sync_channel(8);
    let pending = AtomicBool::new(false);
    for _ in 0..1000 {
        queue_reload(&sender, &pending);
    }
    let (reply, _response) = oneshot::channel();
    assert!(
        sender.try_send(Request::Read(reply)).is_ok(),
        "file notifications must not crowd out user reads or saves"
    );
    assert!(matches!(receiver.try_recv(), Ok(Request::Reload)));
    assert!(matches!(receiver.try_recv(), Ok(Request::Read(_))));
    assert!(receiver.try_recv().is_err());
    pending.store(false, Ordering::Release);
    queue_reload(&sender, &pending);
    assert!(
        matches!(receiver.try_recv(), Ok(Request::Reload)),
        "later filesystem changes must still refresh"
    );
}

#[test]
fn optional_access_port_is_preserved_without_opening_network_or_rewriting_old_config() {
    let old = Config::parse(br#"{"schemaVersion":1}"#).unwrap();
    assert!(old.access.is_none());
    assert!(serde_json::to_value(old).unwrap().get("access").is_none());
    for port in [0, 1024, 65535] {
        let c = Config::parse(
            format!(r#"{{"schemaVersion":1,"access":{{"port":{port}}}}}"#).as_bytes(),
        )
        .unwrap();
        assert_eq!(c.access.unwrap().port, port);
    }
    for raw in [
        r#"{"schemaVersion":1,"access":{"port":80}}"#,
        r#"{"schemaVersion":1,"access":{"port":65536}}"#,
        r#"{"schemaVersion":1,"access":{"enabled":true}}"#,
    ] {
        assert!(Config::parse(raw.as_bytes()).is_err());
    }
}
