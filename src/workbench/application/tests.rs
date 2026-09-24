use super::sources::{LegacySource, SourceWorker};
use super::*;
use crate::workbench::{config::Config, launch::read_entry};
use serde_json::{Value, json};
use std::os::unix::fs::{PermissionsExt, symlink};

struct Fixture {
    dir: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
    paths: WorkbenchPaths,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("项目 with spaces");
        std::fs::create_dir(&cwd).unwrap();
        let home = dir.path().join("missing-native");
        let paths = WorkbenchPaths::from_user_home(dir.path()).unwrap();
        Self {
            dir,
            home,
            cwd,
            paths,
        }
    }
    fn prepared(&self, overrides: Overrides) -> Prepared {
        Prepared::load_application(&self.home, &self.cwd, &self.paths, None, overrides).unwrap()
    }
    fn app(&self, overrides: Overrides) -> Application {
        let prepared = self.prepared(overrides);
        let Acquisition::Owner(lock) =
            InstanceLock::acquire(&self.paths, prepared.config_dir(), &prepared.data_dir).unwrap()
        else {
            panic!("new owner expected")
        };
        Application::start(prepared, self.home.clone(), self.paths.clone(), lock, None).unwrap()
    }
    fn cli(&self) -> PathBuf {
        std::fs::create_dir(&self.home).unwrap();
        std::fs::write(self.home.join("config.toml"), "model=\"synthetic-model\"\nmodel_provider=\"custom\"\n[model_providers.custom]\nname=\"synthetic\"\nbase_url=\"http://127.0.0.1:1/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nexperimental_bearer_token=\"synthetic-token\"\n").unwrap();
        let binary = self.dir.path().join("fake-codex");
        std::fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' invocation >> \"$CODEX_HOME/invocations\"\nif [ \"$1\" = --version ]; then printf 'codex-cli 0.155.1\\n'; exit 0; fi\nprintf '%s\\n' \"$@\" > \"$CODEX_HOME/args\"\nprintf 'synthetic terminal ready\\n'\nexec /bin/cat\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        binary
    }
}
fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}
async fn json_body(response: reqwest::Response) -> Value {
    serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
}
async fn pair(app: &Application) -> String {
    let token = reqwest::Url::parse(&app.entry.url)
        .unwrap()
        .fragment()
        .unwrap()
        .strip_prefix("pair=")
        .unwrap()
        .to_owned();
    let r = client()
        .post(format!("{}/workbench/v1/pair", app.entry.address))
        .header("Origin", &app.entry.address)
        .header("Content-Type", "application/json")
        .body(json!({"token":token}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    r.headers()["set-cookie"].to_str().unwrap().to_owned()
}
async fn get(app: &Application, cookie: &str, path: &str) -> reqwest::Response {
    client()
        .get(format!("{}{path}", app.entry.address))
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn homepage_and_settings_need_no_cli_native_home_or_provider_and_reuse_is_authenticated() {
    let fixture = Fixture::new();
    let missing = fixture.dir.path().join("uninstalled-codex");
    let overrides = Overrides {
        codex_bin: Some(missing.clone()),
        ..Overrides::default()
    };
    let app = fixture.app(overrides.clone());
    let cookie = pair(&app).await;
    let body = json_body(get(&app, &cookie, "/workbench/v1/application").await).await;
    assert!(body["runs"].as_array().unwrap().is_empty());
    assert_eq!(body["instanceId"], app.instance_id.to_string());
    assert!(!fixture.home.exists());
    assert!(!fixture.paths.history_dir().join("runs").exists());
    for _ in 0..5 {
        assert_eq!(get(&app, &cookie, "/").await.status(), 200);
    }
    assert_eq!(
        get(&app, "", "/workbench/v1/application").await.status(),
        401
    );
    assert_eq!(get(&app, &cookie, "/workbench/v1/run").await.status(), 404);
    let saved = json_body(get(&app, &cookie, "/workbench/v1/application/settings").await).await;
    assert_eq!(saved["instanceId"], app.instance_id.to_string());
    assert_eq!(
        saved["settings"]["effective"]["launch"]["codexBin"],
        crate::workbench::config::location_for_application(&missing, &fixture.cwd)
            .unwrap()
            .to_str()
            .unwrap()
    );
    let mut config: Config = serde_json::from_value(saved["settings"]["saved"].clone()).unwrap();
    config.launch.open_browser = false;
    let save = |instance: Uuid| {
        client()
            .put(format!(
                "{}/workbench/v1/application/settings",
                app.entry.address
            ))
            .header("Cookie", &cookie)
            .header("Origin", &app.entry.address)
            .header("Content-Type", "application/json")
            .header(
                "If-Match",
                format!("\"{}\"", saved["settings"]["revision"].as_str().unwrap()),
            )
            .body(json!({"instanceId":instance,"config":config}).to_string())
    };
    assert_eq!(save(Uuid::new_v4()).send().await.unwrap().status(), 409);
    assert_eq!(save(app.instance_id).send().await.unwrap().status(), 200);
    assert_eq!(save(app.instance_id).send().await.unwrap().status(), 412);
    let Acquisition::Existing(path) = InstanceLock::acquire(
        &fixture.paths,
        &fixture.paths.config_dir(),
        &fixture.paths.history_dir(),
    )
    .unwrap() else {
        panic!("must reuse")
    };
    let entry = read_entry(&path).unwrap();
    assert_eq!(entry.format, "codex-view-entry-v2");
    assert_eq!(entry.cli_pid, 0);
    assert!(entry.run_epoch.is_nil());
    assert!(connect(&entry, Connect::default()).await.unwrap().is_none());
    let conflicting = Connect {
        profile: Some("other".into()),
        ..Default::default()
    };
    assert!(
        connect(&entry, conflicting)
            .await
            .unwrap_err()
            .to_string()
            .contains("launch_defaults_conflict")
    );
    let mut stale = entry.clone();
    stale.instance_id = Some(Uuid::new_v4());
    assert!(connect(&stale, Connect::default()).await.is_err());
    assert!(app.healthy());
    let url = app.entry.address.clone();
    drop(app);
    assert!(!path.exists());
    assert!(client().get(url).send().await.is_err());
    assert!(!fixture.home.exists());
}

#[tokio::test]
async fn explicit_launch_runs_once_stops_independently_and_application_drop_reaps_only_its_child() {
    let fixture = Fixture::new();
    let binary = fixture.cli();
    // A broken derived cache must not prevent PTY/Run creation or shutdown.
    let root = crate::workbench::recording::fs::Directory::root(&fixture.paths.root).unwrap();
    root.dir("history", true)
        .unwrap()
        .atomic("library", b"unavailable derived index")
        .unwrap();
    let app = fixture.app(Overrides {
        codex_bin: Some(binary.clone()),
        ..Default::default()
    });
    assert!(!fixture.home.join("invocations").exists());
    let cookie = pair(&app).await;
    let request = Connect {
        project: Some(fixture.cwd.clone()),
        ..Default::default()
    };
    let run = connect(&app.entry, request.clone()).await.unwrap().unwrap();
    let repeated = connect(&app.entry, request).await.unwrap().unwrap();
    assert_eq!(run.run_id, repeated.run_id);
    assert_eq!(run.cli_pid, repeated.cli_pid);
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("invocations"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(get(&app, &cookie, "/workbench/v1/run").await.status(), 404);
    assert_eq!(
        get(
            &app,
            &cookie,
            &format!("/workbench/v1/runs/{}/run", run.run_id)
        )
        .await
        .status(),
        200
    );
    assert!(
        connect(&app.entry, Connect::default())
            .await
            .unwrap()
            .is_none()
    );
    let other = Connect {
        project: Some(fixture.dir.path().to_path_buf()),
        ..Default::default()
    };
    let other = connect(&app.entry, other).await.unwrap().unwrap();
    assert_ne!(other.run_id, run.run_id);
    assert_ne!(other.cli_pid, run.cli_pid);
    let pid = run.cli_pid;
    let response = client()
        .post(format!(
            "{}/workbench/v1/runs/{}/stop",
            app.entry.address, run.run_id
        ))
        .header("Cookie", &cookie)
        .header("Origin", &app.entry.address)
        .header("Content-Type", "application/json")
        .body(json!({"epoch":run.run_id}).to_string())
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let body = json_body(get(&app, &cookie, "/workbench/v1/application").await).await;
            if body["runs"][0]["state"] == "stopped" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    assert_eq!(unsafe { libc::kill(other.cli_pid as i32, 0) }, 0);
    assert!(app.healthy());
    drop(app);
    assert_eq!(unsafe { libc::kill(other.cli_pid as i32, 0) }, -1);
    let app = fixture.app(Overrides {
        codex_bin: Some(binary),
        ..Default::default()
    });
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("invocations"))
            .unwrap()
            .lines()
            .count(),
        4,
        "restart must not replay a Run"
    );
    let restarted = connect(
        &app.entry,
        Connect {
            project: Some(fixture.cwd.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(restarted.run_id, run.run_id);
    assert_ne!(restarted.cli_pid, pid);
    drop(app);
    assert_eq!(
        unsafe { libc::kill(restarted.cli_pid as i32, 0) },
        -1,
        "application shutdown must reap its active child"
    );
}

#[tokio::test]
async fn invalid_config_and_failed_launch_leave_homepage_available_without_overwrite() {
    let fixture = Fixture::new();
    drop(fixture.prepared(Overrides::default()));
    let path = fixture.paths.config_dir().join("config.json");
    std::fs::write(&path, "{ invalid").unwrap();
    let app = fixture.app(Overrides::default());
    let cookie = pair(&app).await;
    let settings = json_body(get(&app, &cookie, "/workbench/v1/application/settings").await).await;
    assert!(settings["settings"]["saved"].is_null());
    assert_eq!(settings["settings"]["errors"][0]["code"], "invalid_config");
    assert!(
        connect(
            &app.entry,
            Connect {
                project: Some(fixture.cwd.clone()),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    assert_eq!(
        get(&app, &cookie, "/workbench/v1/application")
            .await
            .status(),
        200
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "{ invalid");
    assert!(!fixture.home.exists());
}

#[tokio::test]
async fn owner_endpoints_reject_cross_origin_unknown_host_and_wrong_capability() {
    let fixture = Fixture::new();
    let app = fixture.app(Overrides::default());
    let cookie = pair(&app).await;
    for path in [
        "/workbench/v1/application",
        "/workbench/v1/library/sources",
        "/workbench/v1/application/settings",
    ] {
        assert_eq!(get(&app, "workbench_remote=fake", path).await.status(), 401);
        let r = client()
            .get(format!("{}{path}", app.entry.address))
            .header("Cookie", &cookie)
            .header("Host", "evil.invalid")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        let r = client()
            .get(format!("{}{path}", app.entry.address))
            .header("Cookie", &cookie)
            .header("Origin", "http://evil.invalid")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
    }
    let r = client()
        .post(format!(
            "{}/workbench/v1/application/connect",
            app.entry.address
        ))
        .header("Origin", &app.entry.address)
        .header("Content-Type", "application/json")
        .body(
            json!({"instanceId":app.instance_id,"token":"wrong","project":fixture.cwd}).to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert!(!fixture.home.exists());
}

#[test]
fn private_lock_stale_entry_and_replacement_are_identity_checked() {
    let fixture = Fixture::new();
    let prepared = fixture.prepared(Overrides::default());
    let acquire =
        || InstanceLock::acquire(&fixture.paths, prepared.config_dir(), &prepared.data_dir);
    let Acquisition::Owner(lock) = acquire().unwrap() else {
        panic!()
    };
    let path = lock.path();
    std::fs::write(&path, "stale").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(acquire().unwrap(), Acquisition::Existing(_)));
    drop(lock);
    let Acquisition::Owner(lock) = acquire().unwrap() else {
        panic!()
    };
    assert!(!path.exists());
    let original = path.with_file_name("instance.lock");
    std::fs::rename(&original, path.with_file_name("prior.lock")).unwrap();
    std::fs::write(&original, "replacement").unwrap();
    std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(lock.verify().is_err());
    drop(lock);
    symlink(fixture.dir.path().join("unrelated"), &path).unwrap();
    assert!(acquire().is_err());
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn bounded_source_probe_consumes_legacy_reader_off_http_and_does_not_write_source() {
    let fixture = Fixture::new();
    let database = fixture.dir.path().join("old.sqlite");
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch("PRAGMA user_version=25; CREATE TABLE threads(thread_key TEXT, store_source_id TEXT, codex_thread_id TEXT, cwd TEXT, project_key TEXT, name TEXT, last_event_seq INTEGER); INSERT INTO threads VALUES('t','s','n','/synthetic/a','p','Synthetic',1);").unwrap();
    drop(db);
    let before = std::fs::read(&database).unwrap();
    let worker = SourceWorker::start(
        fixture.home.clone(),
        fixture.paths.history_dir(),
        vec![
            LegacySource {
                id: "old".into(),
                database: database.clone(),
                blobs: fixture.dir.path().join("missing-blobs"),
            },
            LegacySource {
                id: "missing".into(),
                database: fixture.dir.path().join("missing.sqlite"),
                blobs: fixture.dir.path().to_path_buf(),
            },
        ],
    )
    .unwrap();
    let state = worker.state();
    let until = std::time::Instant::now() + Duration::from_secs(3);
    while state.read().unwrap().iter().any(|s| s.state == "checking")
        && std::time::Instant::now() < until
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        state
            .read()
            .unwrap()
            .iter()
            .find(|s| s.id == "old")
            .unwrap()
            .state,
        "readable"
    );
    assert_eq!(
        state
            .read()
            .unwrap()
            .iter()
            .find(|s| s.id == "missing")
            .unwrap()
            .state,
        "unavailable"
    );
    drop(worker);
    assert!(state.read().unwrap().iter().all(|s| s.state != "checking"));
    assert_eq!(std::fs::read(&database).unwrap(), before);
    assert!(!database.with_extension("sqlite-wal").exists());
    assert!(!fixture.dir.path().join("missing.sqlite").exists());
}

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGTERM);
        }
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < until {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[tokio::test]
#[ignore = "requires built codex-view and system Chrome; isolated HOME, no CLI or model"]
async fn binary_homepage_reuse_chrome_and_signals_keep_zero_cli() {
    use std::process::{Command, Stdio};
    let fixture = Fixture::new();
    let binary = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/codex-view");
    let entry_path = fixture.dir.path().join("browser-entry.json");
    let launcher = || {
        let mut c = Command::new(&binary);
        c.current_dir(&fixture.cwd)
            .env("HOME", fixture.dir.path())
            .env("USERPROFILE", fixture.dir.path())
            .env("CODEX_HOME", &fixture.home)
            .args(["--no-open", "--codex-bin"])
            .arg(fixture.dir.path().join("missing-cli"))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped());
        c
    };
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let mut child = ChildGuard(
            launcher()
                .arg("--entry-file")
                .arg(&entry_path)
                .spawn()
                .unwrap(),
        );
        let entry = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Ok(entry) = read_entry(&entry_path) {
                    break entry;
                }
                assert!(child.0.try_wait().unwrap().is_none());
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(entry.format, "codex-view-entry-v2");
        for _ in 0..3 {
            let mut repeated = ChildGuard(launcher().spawn().unwrap());
            let until = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(status) = repeated.0.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                assert!(std::time::Instant::now() < until);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(
                read_entry(&entry_path).unwrap().instance_id,
                entry.instance_id
            );
        }
        if signal == libc::SIGINT {
            let mut command = tokio::process::Command::new("node");
            command
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r6-home-probe.cjs"))
                .env("WORKBENCH_PROBE_URL", &entry.url)
                .env("DEBUG", "")
                .env("PWDEBUG", "")
                .stdin(Stdio::piped())
                .stdout(Stdio::inherit())
                .stderr(Stdio::null());
            let extra_native = fixture.dir.path().join("browser-history");
            std::fs::create_dir_all(extra_native.join("sessions")).unwrap();
            std::fs::write(extra_native.join("sessions/synthetic.jsonl"), format!("{}\n{}\n",serde_json::json!({"type":"session_meta","payload":{"id":Uuid::new_v4(),"cwd":fixture.cwd}}),serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"浏览器接入的合成历史"}]}}))).unwrap();
            command.env("WORKBENCH_TEST_HISTORY_SOURCE", &extra_native);
            if let Some(path) = std::env::var_os("WORKBENCH_TEST_SCREENSHOT") {
                command.env("WORKBENCH_PROBE_SCREENSHOT", path);
            }
            let mut probe =
                crate::workbench::probe_process::ProbeProcess::spawn(&mut command).unwrap();
            let _live = probe.stdin.take();
            assert!(
                tokio::time::timeout(Duration::from_secs(50), probe.wait())
                    .await
                    .unwrap()
                    .unwrap()
                    .success()
            );
        }
        assert!(!fixture.home.exists());
        assert!(!fixture.paths.history_dir().join("runs").exists());
        unsafe {
            libc::kill(child.0.id() as i32, signal);
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(!entry_path.exists());
        assert!(client().get(&entry.address).send().await.is_err());
    }
}

#[tokio::test]
async fn library_api_is_owner_only_cached_paginated_and_revocation_scoped() {
    let fixture = Fixture::new();
    let native = fixture.dir.path().join("extra-native");
    std::fs::create_dir_all(native.join("sessions")).unwrap();
    let id = Uuid::new_v4();
    std::fs::write(native.join("sessions/one.jsonl"),format!("{}\n{}\n",serde_json::json!({"type":"session_meta","payload":{"id":id,"cwd":fixture.cwd}}),serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"synthetic searchable"}]}}))).unwrap();
    let app = fixture.app(Overrides::default());
    let cookie = pair(&app).await;
    for path in [
        "/workbench/v1/library/projects",
        "/workbench/v1/library/entries",
        "/workbench/v1/library/entries/e_fake",
        "/workbench/v1/library/entries/e_fake/details",
    ] {
        assert_eq!(get(&app, "", path).await.status(), 401);
    }
    for path in [
        "/workbench/v1/library/refresh",
        "/workbench/v1/library/preview",
    ] {
        assert_eq!(
            client()
                .post(format!("{}{path}", app.entry.address))
                .header("Cookie", &cookie)
                .header("Content-Type", "application/json")
                .body(serde_json::json!({"path":"/missing"}).to_string())
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    let initial = json_body(get(&app, &cookie, "/workbench/v1/library/entries").await).await;
    assert!(initial["records"].as_array().unwrap().is_empty());
    assert!(!fixture.home.exists());
    assert!(!fixture.paths.history_dir().join("runs").exists());
    let settings = json_body(get(&app, &cookie, "/workbench/v1/application/settings").await).await;
    let mut config: Config = serde_json::from_value(settings["settings"]["saved"].clone()).unwrap();
    config
        .history
        .library
        .sources
        .push(crate::history::library::Source::Native {
            id: "extra".into(),
            codex_home: native,
        });
    let saved = client()
        .put(format!(
            "{}/workbench/v1/application/settings",
            app.entry.address
        ))
        .header("Cookie", &cookie)
        .header("Origin", &app.entry.address)
        .header(
            "If-Match",
            format!("\"{}\"", settings["settings"]["revision"].as_str().unwrap()),
        )
        .header("Content-Type", "application/json")
        .body(serde_json::json!({"instanceId":app.instance_id,"config":config}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    let saved = json_body(saved).await;
    let entries = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = json_body(
                get(
                    &app,
                    &cookie,
                    "/workbench/v1/library/entries?q=searchable&limit=1",
                )
                .await,
            )
            .await;
            if value["records"].as_array().is_some_and(|a| a.len() == 1) {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let entry = entries["records"][0]["entryId"].as_str().unwrap();
    assert_eq!(entries["records"][0]["capabilities"]["manage"], false);
    assert_eq!(
        get(
            &app,
            &cookie,
            &format!("/workbench/v1/library/entries/{entry}")
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        get(&app, &cookie, "/workbench/v1/library/entries?limit=201")
            .await
            .status(),
        400
    );
    config.history.library.sources.clear();
    let response = client()
        .put(format!(
            "{}/workbench/v1/application/settings",
            app.entry.address
        ))
        .header("Cookie", &cookie)
        .header("Origin", &app.entry.address)
        .header(
            "If-Match",
            format!("\"{}\"", saved["settings"]["revision"].as_str().unwrap()),
        )
        .header("Content-Type", "application/json")
        .body(serde_json::json!({"instanceId":app.instance_id,"config":config}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        get(
            &app,
            &cookie,
            &format!("/workbench/v1/library/entries/{entry}")
        )
        .await
        .status(),
        404
    );
    assert!(
        json_body(get(&app, &cookie, "/workbench/v1/application").await).await["runs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

include!("launching_tests.rs");
include!("surface_lifecycle_tests.rs");
include!("multi_run_tests.rs");
