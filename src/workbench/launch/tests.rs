//! Synthetic process fixtures verify ownership and failure paths, not CLI compatibility.
use super::*;
use std::os::unix::fs::PermissionsExt;

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
struct Fixture {
    _directory: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
    cli: PathBuf,
    entry: PathBuf,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credential_change_during_final_checks_prevents_cli_spawn() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.home.join("auth.json"),
        r#"{"auth_mode":"apikey","OPENAI_API_KEY":"synthetic-before"}"#,
    )
    .unwrap();
    let result = WorkbenchRuntime::start_checked(fixture.options(), || {
        std::fs::write(
            fixture.home.join("auth.json"),
            r#"{"tokens":{"access_token":"synthetic-after"}}"#,
        )?;
        Ok(())
    })
    .await;
    let error = result.err().expect("credentials changed before spawn");
    assert_eq!(LaunchFailure::from_error(&error), "native_config_changed");
    assert!(!fixture.home.join("fixture.pid").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_launch_uses_saved_defaults_and_explicit_overrides_without_restarting_on_save() {
    use crate::workbench::config::{Config, Overrides, Prepared};
    let fixture = Fixture::new();
    let native_before = std::fs::read(fixture.home.join("config.toml")).unwrap();
    let config_dir = fixture._directory.path().join("preferences");
    let prepared = Prepared::load(
        &fixture.home,
        &fixture.cwd,
        &fixture.paths(),
        Some(&config_dir),
        Overrides::default(),
    )
    .unwrap();
    drop(prepared);
    let mut saved = Config::default();
    saved.launch.codex_bin = fixture.cli.to_str().unwrap().into();
    saved.launch.profile = Some("saved_profile".into());
    std::fs::write(
        config_dir.join("config.json"),
        serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    let prepared = Prepared::load(
        &fixture.home,
        &fixture.cwd,
        &fixture.paths(),
        Some(&config_dir),
        Overrides {
            profile: Some("named".into()),
            open_browser: Some(false),
            ..Overrides::default()
        },
    )
    .unwrap();
    let root = prepared.data_dir.clone();
    assert_eq!(root, fixture.paths().history_dir());
    assert!(
        !fixture.paths().root.exists(),
        "custom config does not create unrelated directories"
    );
    let run = WorkbenchRun::start_configured(
        LaunchOptions {
            cwd: fixture.cwd.clone(),
            home: fixture.home.clone(),
            workbench_paths: fixture.paths(),
            executable: prepared.effective.launch.codex_bin.clone().into(),
            native_profile: prepared.effective.launch.profile.clone(),
            resume: None,
            entry_file: Some(fixture.entry.clone()),
            data_dir: Some(root.clone()),
        },
        prepared,
    )
    .await
    .unwrap();
    let pid = run.process_id();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root
            .join("runs")
            .join(run.epoch.to_string())
            .join("meta.json")
            .exists()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::metadata(&fixture.paths().root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let handle = run._settings.as_ref().unwrap().handle();
    let before = handle.read().await.unwrap();
    assert_eq!(before.effective.launch.profile.as_deref(), Some("named"));
    assert_eq!(before.saved.as_ref().unwrap(), &saved);
    assert!(!before.effective.launch.open_browser);
    saved.launch.profile = Some("future_profile".into());
    saved.storage.data_dir = Some(fixture._directory.path().join("future-history"));
    let after = handle
        .save(before.revision.unwrap(), saved.clone())
        .await
        .unwrap();
    assert_eq!(after.effective.launch.profile.as_deref(), Some("named"));
    assert_eq!(after.effective_data_dir, root);
    assert!(after.restart_required.contains(&"storage.dataDir"));
    assert!(!saved.storage.data_dir.unwrap().exists());
    assert_eq!(run.process_id(), pid);
    assert!(run.healthy() && alive(pid));
    assert_eq!(
        std::fs::read(fixture.home.join("config.toml")).unwrap(),
        native_before
    );
    drop(run);
    assert!(!alive(pid));
}
impl Fixture {
    fn paths(&self) -> WorkbenchPaths {
        WorkbenchPaths::from_user_home(self._directory.path()).unwrap()
    }
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let cwd = directory.path().join("project");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(
            home.join("config.toml"),
            r#"model="synthetic-model"
model_provider="custom"
[model_providers.custom]
name="Synthetic lifecycle fixture"
base_url="http://127.0.0.1:1/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-lifecycle-secret"
"#,
        )
        .unwrap();
        std::fs::write(
            home.join("named.config.toml"),
            "model=\"synthetic-selected\"\n",
        )
        .unwrap();
        let cli = directory.path().join("synthetic-cli");
        script(
            &cli,
            r#"if [ "$1" = "--version" ]; then
  printf 'codex-cli 0.154.0\n'
  exit 0
fi
printf '%s\n' "$@" > "$CODEX_HOME/fixture.args"
printf '%s\n' "${NO_COLOR+set}" "$TERM" "$COLORTERM" > "$CODEX_HOME/fixture.display"
pwd > "$CODEX_HOME/fixture.cwd"
printf '%s\n' "$$" > "$CODEX_HOME/fixture.pid.tmp"
mv "$CODEX_HOME/fixture.pid.tmp" "$CODEX_HOME/fixture.pid"
exec /bin/sleep 60"#,
        );
        let entry = directory.path().join("entry.json");
        Self {
            _directory: directory,
            home,
            cwd,
            cli,
            entry,
        }
    }
    fn options(&self) -> LaunchOptions {
        LaunchOptions {
            cwd: self.cwd.clone(),
            home: self.home.clone(),
            workbench_paths: self.paths(),
            executable: self.cli.clone(),
            native_profile: Some("named".into()),
            resume: Some(uuid::Uuid::new_v4()),
            entry_file: Some(self.entry.clone()),
            data_dir: None,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launch_keeps_home_cwd_native_args_and_cleans_only_owned_resources() {
    let fixture = Fixture::new();
    let config = std::fs::read(fixture.home.join("config.toml")).unwrap();
    let options = fixture.options();
    let resume = options.resume.unwrap();
    let mut unrelated = tokio::process::Command::new("/bin/sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let run = WorkbenchRun::start(options).await.unwrap();
    assert_eq!(run.cli_version, "0.154.0");
    let pid = run.process_id();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !fixture.home.join("fixture.pid").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(run.healthy() && alive(pid));
    let args = std::fs::read_to_string(fixture.home.join("fixture.args")).unwrap();
    let args: Vec<_> = args.lines().collect();
    assert_eq!(args.len(), 8);
    assert_eq!(&args[..3], &["--profile", "named", "-c"]);
    assert!(args[3].starts_with("model_providers.custom.base_url=\"http://127.0.0.1:"));
    assert_eq!(&args[4..6], &["-c", "tui.animations=false"]);
    assert_eq!(&args[6..], &["resume", &resume.to_string()]);
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("fixture.display")).unwrap(),
        "\nxterm-256color\ntruecolor\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("fixture.cwd"))
            .unwrap()
            .trim(),
        fixture.cwd.canonicalize().unwrap().to_str().unwrap()
    );
    let entry = read_entry(&fixture.entry).unwrap();
    let proxy_address = reqwest::Url::parse(&run.runtime.proxy.child_base_url()).unwrap();
    assert_eq!(entry.cli_pid, pid);
    assert_eq!(
        std::fs::read(fixture.home.join("config.toml")).unwrap(),
        config
    );
    drop(run);
    assert!(!alive(pid));
    assert!(!fixture.entry.exists());
    assert!(unrelated.try_wait().unwrap().is_none());
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    assert!(client.get(entry.address).send().await.is_err());
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", proxy_address.port().unwrap()))
            .await
            .is_err()
    );
    unrelated.kill().await.unwrap();
    unrelated.wait().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_error_preserves_existing_entry_and_leaves_no_owned_child() {
    let fixture = Fixture::new();
    std::fs::write(&fixture.entry, "existing user file").unwrap();
    let error = WorkbenchRun::start(fixture.options()).await.err().unwrap();
    assert!(error.to_string().contains("private browser entry"));
    assert_eq!(
        std::fs::read_to_string(&fixture.entry).unwrap(),
        "existing user file"
    );
    if let Ok(pid) = std::fs::read_to_string(fixture.home.join("fixture.pid")) {
        assert!(!alive(pid.trim().parse().unwrap()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn launcher_starts_a_future_cli_without_a_version_override_or_configuration_change() {
    let fixture = Fixture::new();
    let body = std::fs::read_to_string(&fixture.cli)
        .unwrap()
        .replace("0.154.0", "9.0.0");
    std::fs::write(&fixture.cli, body).unwrap();
    let config = std::fs::read(fixture.home.join("config.toml")).unwrap();
    let run = WorkbenchRun::start(fixture.options()).await.unwrap();
    let pid = run.process_id();
    assert_eq!(run.cli_version, "9.0.0");
    assert!(run.healthy() && alive(pid));
    assert_eq!(
        std::fs::read(fixture.home.join("config.toml")).unwrap(),
        config
    );
    drop(run);
    assert!(!alive(pid));
}

#[tokio::test]
async fn version_probe_accepts_other_releases_and_prereleases_without_an_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("version-fixture");
    for release in [
        "0.155.1",
        "0.156.0",
        "1.0.0",
        "0.155.0-alpha.1",
        "0.155.1+build.42",
        "0.153.0",
    ] {
        script(&cli, &format!("printf 'codex-cli {release}\\n'"));
        assert_eq!(version(&cli, dir.path()).await.unwrap(), release);
    }
}

#[tokio::test]
async fn version_probe_rejects_malformed_failed_oversized_and_hanging_output() {
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("version-fixture");
    for body in [
        "printf 'codex-cli 0.1.0 private-marker\\n'",
        "printf 'codex-cli 0.154.0\\n'; exit 1",
        "awk 'BEGIN { for(i=0;i<5000;i++) printf \"x\" }'",
        "exec /bin/sleep 60",
    ] {
        script(&cli, body);
        let before = std::time::Instant::now();
        let error = version(&cli, dir.path()).await.unwrap_err().to_string();
        assert!(!error.contains("private-marker"));
        assert!(before.elapsed() < Duration::from_secs(7));
    }
}
