async fn launch_post(
    app: &Application,
    cookie: &str,
    path: &str,
    body: Value,
) -> reqwest::Response {
    client()
        .post(format!("{}{path}", app.entry.address))
        .header("Cookie", cookie)
        .header("Origin", &app.entry.address)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}
async fn target_for(app: &Application, cookie: &str, path: &Path) -> Value {
    let response = launch_post(
        app,
        cookie,
        "/workbench/v1/launch-targets",
        json!({"path":path}),
    )
    .await;
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(status, 200, "{body}");
    body
}
fn start_body(app: &Application, target: &Value) -> Value {
    json!({"instanceId":app.instance_id,"targetId":target["targetId"],"mode":target["modes"][0],"operationId":Uuid::new_v4(),"configRevision":target["configRevision"]})
}
async fn operation_done(app: &Application, cookie: &str, id: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let response = get(
                app,
                cookie,
                &format!("/workbench/v1/launch-operations/{}", id.as_str().unwrap()),
            )
            .await;
            assert_eq!(response.status(), 200);
            let value = json_body(response).await;
            if value["state"] != "starting" {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
async fn native_entry(app: &Application, cookie: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let body =
                json_body(get(app, cookie, "/workbench/v1/library/entries?kind=native").await)
                    .await;
            if let Some(entry) = body["records"].as_array().and_then(|r| r.first()) {
                break entry.clone();
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn new_directory_with_native_api_key_auth_enters_scoped_workbench() {
    let fixture = Fixture::new();
    let cli = fixture.cli();
    let new_project = fixture.dir.path().join("another 项目 with spaces");
    std::fs::create_dir(&new_project).unwrap();
    std::fs::write(new_project.join("README.md"), "synthetic new project").unwrap();
    let config = std::fs::read_to_string(fixture.home.join("config.toml"))
        .unwrap()
        .replace("requires_openai_auth=false", "requires_openai_auth=true");
    let auth = r#"{"auth_mode":"apikey","OPENAI_API_KEY":"synthetic-ambient-api-key"}"#;
    std::fs::write(fixture.home.join("config.toml"), &config).unwrap();
    std::fs::write(fixture.home.join("auth.json"), auth).unwrap();
    let app = fixture.app(Overrides {
        codex_bin: Some(cli),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let target = target_for(&app, &cookie, &new_project).await;
    let body = start_body(&app, &target);
    assert_eq!(
        launch_post(&app, &cookie, "/workbench/v1/runs", body.clone())
            .await
            .status(),
        202
    );
    let ready = operation_done(&app, &cookie, &body["operationId"]).await;
    assert_eq!(ready["state"], "ready", "{ready}");
    assert_eq!(
        ready["run"]["projectPath"],
        new_project.canonicalize().unwrap().to_str().unwrap()
    );
    let run = ready["run"]["runId"].as_str().unwrap();
    for endpoint in ["run", "live/snapshot", "workspace/files"] {
        assert_eq!(
            get(
                &app,
                &cookie,
                &format!("/workbench/v1/runs/{run}/{endpoint}")
            )
            .await
            .status(),
            200
        );
    }
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("config.toml")).unwrap(),
        config
    );
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("auth.json")).unwrap(),
        auth
    );
}

#[tokio::test]
async fn native_launch_failure_codes_reach_operation_and_homepage_without_payloads() {
    for code in [
        "native_cli_not_found",
        "native_config_invalid",
        "native_auth_unverified",
        "native_provider_unsupported",
        "native_project_config_invalid",
    ] {
        let fixture = Fixture::new();
        let cli = fixture.cli();
        match code {
            "native_cli_not_found" => std::fs::remove_file(&cli).unwrap(),
            "native_config_invalid" => std::fs::write(
                fixture.home.join("config.toml"),
                "secret=[synthetic-private-value",
            )
            .unwrap(),
            "native_auth_unverified" => std::fs::write(
                fixture.home.join("auth.json"),
                r#"{"tokens":{"access_token":"synthetic-private-value"}}"#,
            )
            .unwrap(),
            "native_provider_unsupported" => {
                let text = std::fs::read_to_string(fixture.home.join("config.toml"))
                    .unwrap()
                    .replace("model_provider=\"custom\"", "model_provider=\"other\"");
                std::fs::write(fixture.home.join("config.toml"), text).unwrap();
            }
            _ => {
                std::fs::create_dir(fixture.cwd.join(".codex")).unwrap();
                std::fs::write(
                    fixture.cwd.join(".codex/config.toml"),
                    "cli_auth_credentials_store=\"auto\"\nsecret=\"synthetic-private-value\"",
                )
                .unwrap();
            }
        }
        let app = fixture.app(Overrides {
            codex_bin: Some(cli),
            ..Default::default()
        });
        let cookie = pair(&app).await;
        let target = target_for(&app, &cookie, &fixture.cwd).await;
        let body = start_body(&app, &target);
        assert_eq!(
            launch_post(&app, &cookie, "/workbench/v1/runs", body.clone())
                .await
                .status(),
            202
        );
        let failed = operation_done(&app, &cookie, &body["operationId"]).await;
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["error"]["code"], code);
        let home = json_body(get(&app, &cookie, "/workbench/v1/application").await).await;
        assert_eq!(home["launchError"], code);
        assert_eq!(home["runs"], json!([]));
        assert!(!failed.to_string().contains("synthetic-private-value"));
        assert!(!home.to_string().contains("synthetic-private-value"));
        assert_eq!(get(&app, &cookie, "/").await.status(), 200);
    }
}

#[tokio::test]
async fn web_launch_is_explicit_idempotent_scoped_and_restartable_after_stop() {
    let fixture = Fixture::new();
    let cli = fixture.cli();
    let app = fixture.app(Overrides {
        codex_bin: Some(cli),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let target = target_for(&app, &cookie, &fixture.cwd).await;
    assert_eq!(
        target["canonicalPath"],
        fixture.cwd.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(target["modes"], json!(["new"]));
    assert!(
        !fixture.home.join("invocations").exists(),
        "checking must not even probe CLI version"
    );
    let body = start_body(&app, &target);
    let (first, second) = tokio::join!(
        launch_post(&app, &cookie, "/workbench/v1/runs", body.clone()),
        launch_post(&app, &cookie, "/workbench/v1/runs", body.clone())
    );
    assert_eq!(first.status(), 202);
    assert_eq!(second.status(), 202);
    let ready = operation_done(&app, &cookie, &body["operationId"]).await;
    assert_eq!(ready["state"], "ready", "{ready}");
    let run = ready["run"]["runId"].as_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("invocations"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(
        !std::fs::read_to_string(fixture.home.join("args"))
            .unwrap()
            .lines()
            .any(|arg| arg == "resume")
    );
    let mut conflict = body.clone();
    conflict["mode"] = json!("resume");
    assert_eq!(
        launch_post(&app, &cookie, "/workbench/v1/runs", conflict)
            .await
            .status(),
        409
    );
    let scoped =
        json_body(get(&app, &cookie, &format!("/workbench/v1/runs/{run}/run")).await).await;
    assert_eq!(scoped["runEpoch"], run);
    let files = get(
        &app,
        &cookie,
        &format!("/workbench/v1/runs/{run}/workspace/files"),
    )
    .await;
    assert_eq!(
        files.status(),
        200,
        "scoped workspace route must retain handler dispatch"
    );
    assert_eq!(
        get(
            &app,
            &cookie,
            &format!("/workbench/v1/runs/{}/live/snapshot", Uuid::new_v4())
        )
        .await
        .status(),
        404
    );
    assert_eq!(
        get(&app, "", &format!("/workbench/v1/runs/{run}/live/snapshot"))
            .await
            .status(),
        401
    );
    let again = start_body(&app, &target);
    assert_eq!(
        launch_post(&app, &cookie, "/workbench/v1/runs", again.clone())
            .await
            .status(),
        202
    );
    assert_eq!(
        operation_done(&app, &cookie, &again["operationId"]).await["state"],
        "existing"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("invocations"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(
        launch_post(
            &app,
            &cookie,
            &format!("/workbench/v1/runs/{run}/stop"),
            json!({"epoch":run})
        )
        .await
        .status(),
        202
    );
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let value = json_body(get(&app, &cookie, "/workbench/v1/application").await).await;
            if value["runs"][0]["state"] == "stopped" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let target = target_for(&app, &cookie, &fixture.cwd).await;
    let body = start_body(&app, &target);
    assert_eq!(
        launch_post(&app, &cookie, "/workbench/v1/runs", body.clone())
            .await
            .status(),
        202
    );
    let restarted = operation_done(&app, &cookie, &body["operationId"]).await;
    assert_eq!(restarted["state"], "ready");
    assert_ne!(restarted["run"]["runId"], run);
    assert_eq!(
        get(&app, &cookie, &format!("/workbench/v1/runs/{run}/run"))
            .await
            .status(),
        404
    );
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("invocations"))
            .unwrap()
            .lines()
            .count(),
        4
    );
}

#[tokio::test]
async fn launch_rejects_changed_targets_settings_and_untrusted_http_without_spawning() {
    let fixture = Fixture::new();
    let cli = fixture.cli();
    let app = fixture.app(Overrides {
        codex_bin: Some(cli),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    for body in [
        json!({"path":"relative"}),
        json!({"path":"~someone/project"}),
        json!({"path":fixture.cwd,"projectId":"unassigned"}),
        json!({"path":fixture.cwd,"prompt":"do not send"}),
        json!({"path":fixture.cwd.join("missing")}),
    ] {
        assert!(
            launch_post(&app, &cookie, "/workbench/v1/launch-targets", body)
                .await
                .status()
                .is_client_error()
        );
    }
    let target = target_for(&app, &cookie, &fixture.cwd).await;
    let body = start_body(&app, &target);
    assert_eq!(
        launch_post(&app, "", "/workbench/v1/runs", body.clone())
            .await
            .status(),
        401
    );
    assert_eq!(
        client()
            .post(format!("{}/workbench/v1/runs", app.entry.address))
            .header("Cookie", &cookie)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let mut stale = body.clone();
    stale["instanceId"] = json!(Uuid::new_v4());
    assert_eq!(
        launch_post(&app, &cookie, "/workbench/v1/runs", stale)
            .await
            .status(),
        409
    );
    std::fs::rename(&fixture.cwd, fixture.dir.path().join("old-project")).unwrap();
    std::fs::create_dir(&fixture.cwd).unwrap();
    assert_eq!(
        launch_post(&app, &cookie, "/workbench/v1/runs", body.clone())
            .await
            .status(),
        202
    );
    assert_eq!(
        operation_done(&app, &cookie, &body["operationId"]).await["error"]["code"],
        "project_changed"
    );
    let target = target_for(&app, &cookie, &fixture.cwd).await;
    let body = start_body(&app, &target);
    let config = fixture.paths.config_dir().join("config.json");
    let mut bytes = std::fs::read(&config).unwrap();
    bytes.push(b' ');
    std::fs::write(&config, bytes).unwrap();
    assert_eq!(
        launch_post(&app, &cookie, "/workbench/v1/runs", body.clone())
            .await
            .status(),
        202
    );
    assert_eq!(
        operation_done(&app, &cookie, &body["operationId"]).await["error"]["code"],
        "config_changed"
    );
    assert!(!fixture.home.join("invocations").exists());
    let expanded = json_body(
        launch_post(
            &app,
            &cookie,
            "/workbench/v1/launch-targets",
            json!({"path":"~"}),
        )
        .await,
    )
    .await;
    assert_eq!(
        expanded["canonicalPath"],
        fixture.dir.path().canonicalize().unwrap().to_str().unwrap()
    );
}

#[tokio::test]
async fn explicit_history_resume_uses_session_meta_and_revalidates_source() {
    let fixture = Fixture::new();
    let cli = fixture.cli();
    let sessions = fixture.home.join("sessions/2026/09/22");
    std::fs::create_dir_all(&sessions).unwrap();
    let thread = Uuid::new_v4();
    let file = sessions.join(format!("rollout-2026-09-22T00-00-00-{}.jsonl", thread));
    let raw = format!(
        "{}\n{}\n",
        json!({"type":"session_meta","payload":{"id":thread,"cwd":fixture.cwd,"timestamp":"2026-09-22T00:00:00Z"}}),
        json!({"type":"event_msg","payload":{"type":"user_message","message":"Synthetic saved message, never replay"}})
    );
    std::fs::write(&file, &raw).unwrap();
    let app = fixture.app(Overrides {
        codex_bin: Some(cli),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let entry = native_entry(&app, &cookie).await;
    let selection = json!({"projectId":entry["projectId"],"resumeEntryId":entry["entryId"],"sourceRevision":entry["sourceRevision"]});
    let response = launch_post(
        &app,
        &cookie,
        "/workbench/v1/launch-targets",
        selection.clone(),
    )
    .await;
    assert_eq!(response.status(), 200);
    let target = json_body(response).await;
    assert_eq!(target["modes"], json!(["resume"]));
    assert!(!fixture.home.join("invocations").exists());
    let body = start_body(&app, &target);
    assert_eq!(
        launch_post(&app, &cookie, "/workbench/v1/runs", body.clone())
            .await
            .status(),
        202
    );
    let ready = operation_done(&app, &cookie, &body["operationId"]).await;
    assert_eq!(ready["state"], "ready", "{ready}");
    let args = std::fs::read_to_string(fixture.home.join("args")).unwrap();
    assert!(args.ends_with(&format!("resume\n{thread}\n")));
    assert!(!args.contains("Synthetic saved"));
    assert!(!args.contains("--last"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), raw);
    let repeat = start_body(&app, &target);
    launch_post(&app, &cookie, "/workbench/v1/runs", repeat.clone()).await;
    assert_eq!(
        operation_done(&app, &cookie, &repeat["operationId"]).await["error"]["code"],
        "project_session_running"
    );
    std::fs::write(&file, raw.replace("Synthetic saved", "Changed saved")).unwrap();
    assert_eq!(
        launch_post(&app, &cookie, "/workbench/v1/launch-targets", selection)
            .await
            .status(),
        409
    );
    let next = start_body(&app, &target);
    assert_eq!(
        launch_post(&app, &cookie, "/workbench/v1/runs", next.clone())
            .await
            .status(),
        202
    );
    assert_eq!(
        operation_done(&app, &cookie, &next["operationId"]).await["error"]["code"],
        "source_revision_changed"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("invocations"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[tokio::test]
async fn direct_resume_rejects_other_project_and_unlocatable_metadata_before_cli_probe() {
    let fixture = Fixture::new();
    let cli = fixture.cli();
    let sessions = fixture.home.join("sessions/2026/09/22");
    std::fs::create_dir_all(&sessions).unwrap();
    let thread = Uuid::new_v4();
    let file = sessions.join(format!("rollout-2026-09-22T00-00-00-{thread}.jsonl"));
    std::fs::write(&file, format!("{}\n", json!({"type":"session_meta","payload":{"id":thread,"cwd":fixture.dir.path(),"timestamp":"2026-09-22T00:00:00Z"}}))).unwrap();
    let app = fixture.app(Overrides {
        codex_bin: Some(cli),
        ..Default::default()
    });
    let request = Connect {
        project: Some(fixture.cwd.clone()),
        resume: Some(thread),
        ..Default::default()
    };
    assert!(connect(&app.entry, request.clone()).await.is_err());
    assert!(!fixture.home.join("invocations").exists());
    std::fs::write(&file, format!("{}\n", json!({"type":"session_meta","payload":{"id":Uuid::new_v4(),"cwd":fixture.cwd,"timestamp":"2026-09-22T00:00:00Z"}}))).unwrap();
    assert!(connect(&app.entry, request).await.is_err());
    assert!(!fixture.home.join("invocations").exists());
}

#[tokio::test]
async fn proxy_failure_stops_live_cli_before_slot_can_restart() {
    let fixture = Fixture::new();
    let cli = fixture.cli();
    let app = fixture.app(Overrides {
        codex_bin: Some(cli),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let target = target_for(&app, &cookie, &fixture.cwd).await;
    let body = start_body(&app, &target);
    launch_post(&app, &cookie, "/workbench/v1/runs", body.clone()).await;
    let ready = operation_done(&app, &cookie, &body["operationId"]).await;
    assert_eq!(ready["state"], "ready");
    let pid = ready["run"]["cliPid"].as_u64().unwrap() as i32;
    let (tx, rx) = oneshot::channel();
    app._runs
        .handle
        .sender
        .send(Request::Inspect(Box::new(move |runner| {
            let run = &runner.active.first().unwrap().0;
            assert!(!run.terminal().exited());
            run.abort_proxy_for_test();
            let _ = tx.send(());
        })))
        .unwrap();
    rx.await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = json_body(get(&app, &cookie, "/workbench/v1/application").await).await;
            if value["runs"][0]["state"] == "stopped" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "stopped must mean CLI has exited"
    );
    let next = start_body(&app, &target);
    launch_post(&app, &cookie, "/workbench/v1/runs", next.clone()).await;
    assert_eq!(
        operation_done(&app, &cookie, &next["operationId"]).await["state"],
        "ready"
    );
    assert!(app.healthy());
}

#[tokio::test]
async fn spawn_rechecks_directory_after_a_slow_native_version_probe() {
    let fixture = Fixture::new();
    let cli = fixture.cli();
    let raw = std::fs::read_to_string(&cli).unwrap().replace("then printf 'codex-cli", "then touch \"$CODEX_HOME/probing\"; while [ ! -f \"$CODEX_HOME/continue\" ]; do sleep 0.05; done; printf 'codex-cli");
    std::fs::write(&cli, raw).unwrap();
    let app = fixture.app(Overrides {
        codex_bin: Some(cli),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let target = target_for(&app, &cookie, &fixture.cwd).await;
    let body = start_body(&app, &target);
    launch_post(&app, &cookie, "/workbench/v1/runs", body.clone()).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while !fixture.home.join("probing").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Queries stay available while the serialized worker probes the CLI.
    assert_eq!(
        json_body(
            get(
                &app,
                &cookie,
                &format!(
                    "/workbench/v1/launch-operations/{}",
                    body["operationId"].as_str().unwrap()
                )
            )
            .await
        )
        .await["state"],
        "starting"
    );
    std::fs::rename(&fixture.cwd, fixture.dir.path().join("replaced")).unwrap();
    std::fs::create_dir(&fixture.cwd).unwrap();
    std::fs::write(fixture.home.join("continue"), b"").unwrap();
    let result = operation_done(&app, &cookie, &body["operationId"]).await;
    assert_eq!(result["error"]["code"], "project_changed");
    assert!(
        !fixture.home.join("args").exists(),
        "version probe must not be followed by PTY spawn"
    );
}

#[tokio::test]
async fn native_folder_picker_is_owner_only_explicit_and_never_launches_cli() {
    let fixture = Fixture::new();
    let cli = fixture.cli();
    let app = fixture.app(Overrides {
        codex_bin: Some(cli),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let program = fixture.dir.path().join("fake-picker");
    let output = fixture.dir.path().join("chosen.json");
    std::fs::write(&output, serde_json::to_vec(&fixture.cwd).unwrap()).unwrap();
    let quote = |s: &Path| format!("'{}'", s.to_str().unwrap().replace('\'', "'\\''"));
    std::fs::write(
        &program,
        format!("#!/bin/sh\nexec /bin/cat {}\n", quote(&output)),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    app.web.set_picker_program_for_test(program);
    let endpoint = "/workbench/v1/application/pick-directory";
    let body = json!({"instanceId": app.instance_id});
    assert_eq!(
        launch_post(&app, "", endpoint, body.clone()).await.status(),
        401
    );
    assert_eq!(
        client()
            .post(format!("{}{endpoint}", app.entry.address))
            .header("Cookie", &cookie)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        launch_post(
            &app,
            &cookie,
            endpoint,
            json!({"instanceId":Uuid::new_v4()})
        )
        .await
        .status(),
        409
    );
    assert_eq!(
        launch_post(
            &app,
            &cookie,
            endpoint,
            json!({"instanceId":app.instance_id,"script":"untrusted"})
        )
        .await
        .status(),
        400
    );
    let selected = launch_post(&app, &cookie, endpoint, body.clone()).await;
    assert_eq!(selected.status(), 200);
    assert_eq!(
        json_body(selected).await["path"],
        fixture.cwd.to_str().unwrap()
    );
    std::fs::write(&output, b"null").unwrap();
    assert!(json_body(launch_post(&app, &cookie, endpoint, body).await).await["path"].is_null());
    assert!(!fixture.home.join("invocations").exists());
    assert!(
        json_body(get(&app, &cookie, "/workbench/v1/application").await).await["runs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn projectless_resume_target_uses_recorded_cwd_without_a_path_selector() {
    let fixture = Fixture::new();
    let cli = fixture.cli();
    let cwd = fixture
        .dir
        .path()
        .join("Documents/Codex/2026-09-23/new-chat");
    std::fs::create_dir_all(&cwd).unwrap();
    let sessions = fixture.home.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let thread = Uuid::new_v4();
    std::fs::write(sessions.join(format!("rollout-2026-09-23T00-00-00-{thread}.jsonl")), format!("{}\n", json!({"type":"session_meta","payload":{"id":thread,"cwd":cwd,"originator":"Codex Desktop"}}))).unwrap();
    let app = fixture.app(Overrides {
        codex_bin: Some(cli),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let entry = native_entry(&app, &cookie).await;
    assert!(entry["projectId"].is_null());
    let selection =
        json!({"resumeEntryId":entry["entryId"],"sourceRevision":entry["sourceRevision"]});
    let response = launch_post(
        &app,
        &cookie,
        "/workbench/v1/launch-targets",
        selection.clone(),
    )
    .await;
    assert_eq!(response.status(), 200);
    let target = json_body(response).await;
    assert_eq!(
        target["canonicalPath"],
        cwd.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(target["modes"], json!(["resume"]));
    let mut compatible = selection.clone();
    compatible["path"] = json!(cwd);
    assert_eq!(
        launch_post(&app, &cookie, "/workbench/v1/launch-targets", compatible)
            .await
            .status(),
        200
    );
    for extra in [
        json!({"path":fixture.cwd}),
        json!({"path":"."}),
        json!({"path":"../."}),
        json!({"projectId":"p_not_this_project"}),
        json!({"path":cwd,"projectId":"p_not_this_project"}),
    ] {
        let mut invalid = selection.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(
            launch_post(&app, &cookie, "/workbench/v1/launch-targets", invalid)
                .await
                .status()
                .is_client_error()
        );
    }
    for invalid in [
        json!({}),
        json!({"resumeEntryId":entry["entryId"]}),
        json!({"sourceRevision":entry["sourceRevision"]}),
    ] {
        assert_eq!(
            launch_post(&app, &cookie, "/workbench/v1/launch-targets", invalid)
                .await
                .status(),
            422
        );
    }
    assert!(!fixture.home.join("invocations").exists());
}
