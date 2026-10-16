use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

fn fixture() -> (tempfile::TempDir, Manager) {
    let dir = tempfile::tempdir().unwrap();
    let paths = WorkbenchPaths::from_user_home(dir.path()).unwrap();
    let manager = Manager::new(&paths, &paths.config_dir(), &paths.history_dir()).unwrap();
    (dir, manager)
}

#[test]
fn plist_escapes_arguments_and_persists_only_explicit_nonsecret_environment() {
    let (dir, m) = fixture();
    let mut spec = m
        .specification(
            dir.path().join("native"),
            &Overrides {
                profile: Some("a < & ' \" 中文".into()),
                ..Default::default()
            },
        )
        .unwrap();
    spec.search_path = "/usr/bin:/a & b".into();
    let p = m.plist(&spec).unwrap();
    assert!(p.contains("a &lt; &amp; &apos; &quot; 中文"));
    assert!(p.contains("<key>HOME</key>"));
    assert!(p.contains("<key>CODEX_HOME</key>"));
    assert!(p.contains("<key>Umask</key><integer>63</integer>"));
    assert!(p.contains("<key>SuccessfulExit</key><false/>"));
    assert!(!p.contains("--project"));
    assert!(!p.contains("OPENAI_API_KEY"));
    assert!(!p.contains("TOKEN"));
    m.directory(true)
        .unwrap()
        .atomic(SPEC, &serde_json::to_vec(&spec).unwrap())
        .unwrap();
    assert!(m.saved().unwrap().is_some());
    assert_eq!(
        fs::metadata(m.directory_path().join(SPEC))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn scoped_service_lock_is_exclusive_and_reusable() {
    let (_dir, m) = fixture();
    let first = m.lock().unwrap();
    assert!(m.lock().is_err());
    drop(first);
    assert!(m.lock().is_ok());
}

#[test]
fn instance_probe_ignores_stale_entries_but_does_not_remove_them() {
    let (_dir, m) = fixture();
    assert!(m.running_entry().unwrap().is_none());
    let d = m.directory(true).unwrap();
    d.open("instance.lock", true).unwrap();
    d.atomic("instance.json", b"not a real private entry")
        .unwrap();
    assert!(m.running_entry().unwrap().is_none());
    assert_eq!(
        d.read("instance.json", 100).unwrap(),
        b"not a real private entry"
    );
}

#[test]
fn altered_login_files_and_linked_parents_are_never_overwritten() {
    let (dir, m) = fixture();
    let s = m
        .specification(dir.path().join("native"), &Overrides::default())
        .unwrap();
    m.directory(true)
        .unwrap()
        .atomic(SPEC, &serde_json::to_vec(&s).unwrap())
        .unwrap();
    m.write_login_plist(&s).unwrap();
    assert!(m.autostart_enabled().unwrap());
    fs::write(m.autostart_path(), "user changed this").unwrap();
    assert!(m.autostart_enabled().is_err());
    assert_eq!(
        fs::read_to_string(m.autostart_path()).unwrap(),
        "user changed this"
    );
    fs::remove_file(m.autostart_path()).unwrap();
    let agents = dir.path().join("Library/LaunchAgents");
    fs::remove_dir(&agents).unwrap();
    symlink(dir.path(), &agents).unwrap();
    assert!(m.write_login_plist(&s).is_err());
}

#[test]
fn manifest_requires_exact_scope_and_safe_arguments() {
    let (dir, m) = fixture();
    let mut s = m
        .specification(dir.path().join("native"), &Overrides::default())
        .unwrap();
    s.label = "another job".into();
    assert!(m.validate(&s).is_err());
    s.label = m.label();
    s.arguments.push("unsafe\nargument".into());
    assert!(m.validate(&s).is_err());
    assert!(text_path(Path::new("a\npath")).is_err());
}

fn register(manager: &Manager, native: &Path, autostart: bool) -> Specification {
    let spec = manager
        .specification(native.into(), &Overrides::default())
        .unwrap();
    let d = manager.directory(true).unwrap();
    d.atomic(SPEC, &serde_json::to_vec(&spec).unwrap()).unwrap();
    d.atomic(PLIST, manager.plist(&spec).unwrap().as_bytes())
        .unwrap();
    if autostart {
        manager.write_login_plist(&spec).unwrap();
    }
    spec
}

#[test]
fn plain_start_reuses_saved_defaults_and_explicit_conflicts_are_detected() {
    let (dir, manager) = fixture();
    let saved = manager
        .specification(
            dir.path().join("native"),
            &Overrides {
                profile: Some("saved-profile".into()),
                provider_profile: Some("saved-provider".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let plain = manager
        .specification(dir.path().join("native"), &Overrides::default())
        .unwrap();
    assert!(compatible_saved(&saved, &plain).is_ok());
    let reused = manager
        .inactive_specification(plain, Some(&saved), true)
        .unwrap();
    assert!(reused == saved);
    let mut explicit = manager
        .specification(
            dir.path().join("native"),
            &Overrides {
                profile: Some("new-profile".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(compatible_saved(&saved, &explicit).is_err());
    assert!(
        manager
            .inactive_specification(explicit.clone(), Some(&saved), true)
            .is_err()
    );
    let changed = manager
        .inactive_specification(explicit.clone(), Some(&saved), false)
        .unwrap();
    assert_eq!(argument(&changed, "--profile"), Some("new-profile"));
    assert_eq!(
        argument(&changed, "--provider-profile"),
        Some("saved-provider")
    );
    explicit.native_home = dir.path().join("other-native");
    assert!(compatible_saved(&saved, &explicit).is_err());
    explicit.native_home = saved.native_home.clone();
    explicit.executable = dir.path().join("different-binary");
    assert!(compatible_saved(&saved, &explicit).is_err());
}

#[test]
fn inactive_autostart_conflict_preserves_all_registered_files() {
    let (dir, manager) = fixture();
    let saved = register(&manager, &dir.path().join("native"), true);
    let requested = manager
        .specification(dir.path().join("new-native"), &Overrides::default())
        .unwrap();
    let before = fs::read(manager.autostart_path()).unwrap();
    assert!(
        manager
            .inactive_specification(requested, Some(&saved), true)
            .is_err()
    );
    assert!(manager.saved().unwrap().unwrap() == saved);
    assert_eq!(fs::read(manager.autostart_path()).unwrap(), before);
    assert!(manager.check_runtime_plist(&saved).unwrap());
}

#[test]
fn autostart_enable_validates_retained_identity_and_explicit_defaults() {
    let (dir, manager) = fixture();
    let saved = manager
        .specification(
            dir.path().join("native"),
            &Overrides {
                codex_bin: Some(dir.path().join("codex")),
                profile: Some("saved-profile".into()),
                provider_profile: Some("saved-provider".into()),
                ..Default::default()
            },
        )
        .unwrap();
    manager
        .directory(true)
        .unwrap()
        .atomic(SPEC, &serde_json::to_vec(&saved).unwrap())
        .unwrap();
    let plain = manager
        .specification(dir.path().join("native"), &Overrides::default())
        .unwrap();
    assert!(
        manager
            .autostart_specification(true, plain.clone())
            .unwrap()
            == saved
    );
    let mut changed = plain.clone();
    changed.executable = dir.path().join("different-executable");
    assert!(
        manager
            .autostart_specification(true, changed.clone())
            .is_err()
    );
    // Turning off old login startup remains possible from a replacement binary.
    assert!(manager.autostart_specification(false, changed).unwrap() == saved);
    let mut changed = plain.clone();
    changed.native_home = dir.path().join("other-native");
    assert!(manager.autostart_specification(true, changed).is_err());
    for overrides in [
        Overrides {
            codex_bin: Some(dir.path().join("different-codex")),
            ..Default::default()
        },
        Overrides {
            profile: Some("different-profile".into()),
            ..Default::default()
        },
        Overrides {
            provider_profile: Some("different-provider".into()),
            ..Default::default()
        },
    ] {
        let changed = manager
            .specification(dir.path().join("native"), &overrides)
            .unwrap();
        assert!(manager.autostart_specification(true, changed).is_err());
    }
    assert!(manager.saved().unwrap().unwrap() == saved);
    assert!(!manager.autostart_path().exists());
}

#[test]
fn private_log_is_created_before_first_autostart_and_unsafe_logs_rejected() {
    let (dir, manager) = fixture();
    manager.ensure_log().unwrap();
    let log = manager.directory_path().join("service.log");
    assert_eq!(fs::metadata(&log).unwrap().mode() & 0o777, 0o600);
    fs::remove_file(&log).unwrap();
    let other = dir.path().join("other");
    fs::write(&other, "preserve this").unwrap();
    symlink(&other, &log).unwrap();
    assert!(manager.ensure_log().is_err());
    assert_eq!(fs::read_to_string(other).unwrap(), "preserve this");
}

#[test]
fn installation_reservation_blocks_installed_binaries_but_not_source_builds() {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("bin/codex-view");
    assert!(installation_available(&executable).is_ok());
    fs::create_dir(dir.path().join(".code-view-install-lock")).unwrap();
    assert!(installation_available(&executable).is_err());
    assert!(installation_available(&dir.path().join("target/debug/codex-view")).is_ok());
}

#[test]
fn kickstart_timeout_covers_the_configured_spawn_throttle() {
    let (dir, manager) = fixture();
    let spec = manager
        .specification(dir.path().join("native"), &Overrides::default())
        .unwrap();
    assert!(manager.plist(&spec).unwrap().contains(&format!(
        "<key>ThrottleInterval</key><integer>{LAUNCHD_THROTTLE_SECONDS}</integer>"
    )));
    let kickstart = launchctl_timeout(&["kickstart".into(), manager.target()]);
    assert!(kickstart >= Duration::from_secs(LAUNCHD_THROTTLE_SECONDS + 10));
    assert!(
        manager
            .plist(&spec)
            .unwrap()
            .contains("<key>ExitTimeOut</key><integer>30</integer>")
    );
    assert_eq!(
        launchctl_timeout(&["bootout".into(), manager.target()]),
        Duration::from_secs(40)
    );
    for command in ["print", "enable", "bootstrap"] {
        assert_eq!(launchctl_timeout(&[command.into()]), Duration::from_secs(8));
    }
}

#[test]
fn lookup_finds_saved_scope_even_with_malformed_current_config() {
    let (dir, manager) = fixture();
    let paths = WorkbenchPaths::from_user_home(dir.path()).unwrap();
    let custom = Manager::new(
        &paths,
        &paths.config_dir(),
        &paths.root.join("custom-history"),
    )
    .unwrap();
    register(&custom, &dir.path().join("native"), false);
    Directory::root(&paths.config_dir())
        .unwrap()
        .atomic("config.json", b"broken settings")
        .unwrap();
    let found = Manager::lookup(&paths, None, None, dir.path()).unwrap();
    assert_eq!(found.scope, custom.scope);
    let explicit = Manager::lookup(&paths, None, Some(&manager.data), dir.path()).unwrap();
    assert_eq!(explicit.scope, manager.scope);
}

#[test]
fn lookup_requires_data_directory_when_multiple_saved_scopes_match() {
    let (dir, manager) = fixture();
    let paths = WorkbenchPaths::from_user_home(dir.path()).unwrap();
    let other = Manager::new(
        &paths,
        &paths.config_dir(),
        &paths.root.join("other-history"),
    )
    .unwrap();
    register(&manager, &dir.path().join("native"), false);
    register(&other, &dir.path().join("native"), false);
    assert!(
        Manager::lookup(&paths, None, None, dir.path())
            .err()
            .unwrap()
            .to_string()
            .contains("--data-dir")
    );
    assert_eq!(
        Manager::lookup(&paths, None, Some(&other.data), dir.path())
            .unwrap()
            .scope,
        other.scope
    );
}

#[test]
fn lookup_uses_valid_foreground_data_hint_and_tolerates_broken_settings() {
    let (dir, manager) = fixture();
    let paths = WorkbenchPaths::from_user_home(dir.path()).unwrap();
    Directory::root(&paths.root).unwrap();
    let config_dir = Directory::root(&paths.config_dir()).unwrap();
    let mut config = super::super::super::config::Config::default();
    config.storage.data_dir = Some(dir.path().join("foreground-history"));
    config_dir
        .atomic("config.json", &serde_json::to_vec(&config).unwrap())
        .unwrap();
    let expected_data = super::super::super::config::location_for_application(
        config.storage.data_dir.as_deref().unwrap(),
        dir.path(),
    )
    .unwrap();
    let expected = Manager::new(&paths, &paths.config_dir(), &expected_data).unwrap();
    assert_eq!(
        Manager::lookup(&paths, None, None, dir.path())
            .unwrap()
            .scope,
        expected.scope
    );
    config_dir.atomic("config.json", b"not JSON").unwrap();
    assert_eq!(
        Manager::lookup(&paths, None, None, dir.path())
            .unwrap()
            .scope,
        manager.scope
    );
}

#[tokio::test]
async fn installer_rejects_modified_or_symlink_runtime_plist_without_removing_anything() {
    let (dir, manager) = fixture();
    let paths = WorkbenchPaths::from_user_home(dir.path()).unwrap();
    register(&manager, &dir.path().join("native"), true);
    let runtime = manager.directory_path().join(PLIST);
    fs::write(&runtime, "altered runtime plist").unwrap();
    assert!(installation_guard(&paths, true).await.is_err());
    assert!(manager.autostart_path().exists());
    assert!(manager.saved().unwrap().is_some());
    assert_eq!(
        fs::read_to_string(&runtime).unwrap(),
        "altered runtime plist"
    );
    fs::remove_file(&runtime).unwrap();
    let target = dir.path().join("foreign.plist");
    fs::write(&target, "must survive").unwrap();
    symlink(&target, &runtime).unwrap();
    assert!(installation_guard(&paths, true).await.is_err());
    assert!(manager.autostart_path().exists());
    assert!(manager.saved().unwrap().is_some());
    assert_eq!(fs::read_to_string(target).unwrap(), "must survive");
}

#[tokio::test]
async fn installer_preflights_later_active_scope_before_removing_earlier_scope() {
    let (dir, manager) = fixture();
    let paths = WorkbenchPaths::from_user_home(dir.path()).unwrap();
    let other = Manager::new(
        &paths,
        &paths.root.join("other-config"),
        &paths.root.join("other-history"),
    )
    .unwrap();
    let (first, later) = if manager.scope < other.scope {
        (manager, other)
    } else {
        (other, manager)
    };
    register(&first, &dir.path().join("native"), true);
    register(&later, &dir.path().join("native"), false);
    let d = later.directory(true).unwrap();
    let active_lock = d.open("instance.lock", true).unwrap();
    assert_eq!(
        unsafe { libc::flock(active_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let entry = BrowserEntry {
        format: "codex-view-entry-v2".into(),
        instance_id: Some(Uuid::new_v4()),
        address: "http://127.0.0.1:12345".into(),
        url: format!("http://127.0.0.1:12345/#pair={}", "a".repeat(64)),
        ..Default::default()
    };
    d.atomic("instance.json", &serde_json::to_vec(&entry).unwrap())
        .unwrap();
    assert!(installation_guard(&paths, true).await.is_err());
    assert!(first.saved().unwrap().is_some());
    assert!(first.autostart_path().exists());
    assert!(first.directory_path().join(PLIST).exists());
    assert!(later.saved().unwrap().is_some());
    drop(active_lock);
}

#[tokio::test]
async fn installer_removes_only_fully_verified_inactive_registration() {
    let (dir, manager) = fixture();
    let paths = WorkbenchPaths::from_user_home(dir.path()).unwrap();
    register(&manager, &dir.path().join("native"), true);
    installation_guard(&paths, true).await.unwrap();
    assert!(!manager.directory_path().join(SPEC).exists());
    assert!(!manager.directory_path().join(PLIST).exists());
    assert!(!manager.autostart_path().exists());
}

#[tokio::test]
async fn installer_cannot_overtake_first_start_before_spec_is_written() {
    let (dir, manager) = fixture();
    let paths = WorkbenchPaths::from_user_home(dir.path()).unwrap();
    let starting = manager.lock().unwrap();
    assert!(manager.saved().unwrap().is_none());
    assert!(installation_guard(&paths, false).await.is_err());
    assert!(installation_guard(&paths, true).await.is_err());
    assert!(manager.saved().unwrap().is_none());
    drop(starting);
    installation_guard(&paths, false).await.unwrap();
}
