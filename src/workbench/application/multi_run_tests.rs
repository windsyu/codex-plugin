async fn run_state(app: &Application, cookie: &str, id: &str, expected: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let body = json_body(get(app, cookie, &format!("/workbench/v1/runs/{id}")).await).await;
            if body["state"] == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Run state must converge");
}

#[tokio::test]
async fn parallel_projects_have_scoped_routes_capacity_and_local_failure_cleanup() {
    let fixture = Fixture::new();
    let cli = fixture.cli();
    // A separately owned CLI must survive both a Run failure and Application exit.
    let external_home = fixture.dir.path().join("external-native");
    std::fs::create_dir(&external_home).unwrap();
    let mut external = ChildGuard(
        std::process::Command::new(&cli)
            .env("CODEX_HOME", &external_home)
            .env("HOME", fixture.dir.path())
            .env("USERPROFILE", fixture.dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let app = fixture.app(Overrides {
        codex_bin: Some(cli),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let mut runs = Vec::new();
    for index in 0..MAX_RUNNING_RUNS {
        let path = fixture.dir.path().join(format!("并行项目 {index}"));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join(format!("project-{index}.txt")),
            format!("独立文件 {index}"),
        )
        .unwrap();
        let target = target_for(&app, &cookie, &path).await;
        assert_eq!(target["openInNewTab"], index > 0);
        let body = start_body(&app, &target);
        assert_eq!(
            launch_post(&app, &cookie, "/workbench/v1/runs", body.clone())
                .await
                .status(),
            202
        );
        let operation = operation_done(&app, &cookie, &body["operationId"]).await;
        assert_eq!(operation["state"], "ready", "{operation}");
        assert_eq!(operation["openInNewTab"], index > 0);
        let run = operation["run"].clone();
        let id = run["runId"].as_str().unwrap();
        assert!(
            runs.iter()
                .all(|previous: &Value| previous["runId"] != run["runId"]
                    && previous["cliPid"] != run["cliPid"])
        );
        let snapshot = json_body(
            get(
                &app,
                &cookie,
                &format!("/workbench/v1/runs/{id}/live/snapshot"),
            )
            .await,
        )
        .await;
        assert_eq!(snapshot["runEpoch"], id);
        let files = get(
            &app,
            &cookie,
            &format!("/workbench/v1/runs/{id}/workspace/files"),
        )
        .await;
        assert_eq!(files.status(), 200);
        let files = files.text().await.unwrap();
        assert!(files.contains(&format!("project-{index}.txt")));
        for other in 0..MAX_RUNNING_RUNS {
            if other != index {
                assert!(!files.contains(&format!("project-{other}.txt")));
            }
        }
        // Repeating the operation and selecting the same project are distinct
        // paths, and neither creates a second CLI.
        launch_post(&app, &cookie, "/workbench/v1/runs", body.clone()).await;
        assert_eq!(
            operation_done(&app, &cookie, &body["operationId"]).await["run"],
            run
        );
        let again = start_body(&app, &target);
        launch_post(&app, &cookie, "/workbench/v1/runs", again.clone()).await;
        assert_eq!(
            operation_done(&app, &cookie, &again["operationId"]).await["state"],
            "existing"
        );
        runs.push(run);
    }
    let target = target_for(&app, &cookie, &fixture.cwd).await;
    let body = start_body(&app, &target);
    launch_post(&app, &cookie, "/workbench/v1/runs", body.clone()).await;
    let failed = operation_done(&app, &cookie, &body["operationId"]).await;
    assert_eq!(failed["error"]["code"], "run_capacity");
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("invocations"))
            .unwrap()
            .lines()
            .count(),
        MAX_RUNNING_RUNS * 2
    );
    for path in [
        "run",
        "terminal",
        "live/snapshot",
        "workspace/files",
        "settings",
        "history",
    ] {
        assert_eq!(
            get(&app, &cookie, &format!("/workbench/v1/{path}"))
                .await
                .status(),
            404
        );
    }
    let a: Uuid = serde_json::from_value(runs[0]["runId"].clone()).unwrap();
    let (tx, rx) = oneshot::channel();
    app._runs
        .handle
        .sender
        .send(Request::Inspect(Box::new(move |runner| {
            runner
                .active
                .iter()
                .find(|(_, _, summary)| summary.run_id == a)
                .unwrap()
                .0
                .abort_proxy_for_test();
            let _ = tx.send(());
        })))
        .unwrap();
    rx.await.unwrap();
    run_state(&app, &cookie, runs[0]["runId"].as_str().unwrap(), "stopped").await;
    for run in &runs[1..] {
        assert_eq!(
            unsafe { libc::kill(run["cliPid"].as_i64().unwrap() as i32, 0) },
            0
        );
        assert_eq!(
            json_body(
                get(
                    &app,
                    &cookie,
                    &format!("/workbench/v1/runs/{}", run["runId"].as_str().unwrap())
                )
                .await
            )
            .await["state"],
            "running"
        );
    }
    assert!(app.healthy());
    drop(app);
    for run in runs {
        assert_eq!(
            unsafe { libc::kill(run["cliPid"].as_i64().unwrap() as i32, 0) },
            -1
        );
    }
    assert!(
        external.0.try_wait().unwrap().is_none(),
        "separately owned CLI must remain alive"
    );
}

#[tokio::test]
async fn an_observed_native_thread_cannot_be_resumed_in_another_live_project() {
    use crate::workbench::decode::{
        Change, Decoded,
        request::{PurposeBasis, RequestInfo, RequestPurpose},
    };
    let fixture = Fixture::new();
    let app = fixture.app(Overrides {
        codex_bin: Some(fixture.cli()),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let a = connect(
        &app.entry,
        Connect {
            project: Some(fixture.cwd.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let b = fixture.dir.path().join("thread-origin");
    std::fs::create_dir(&b).unwrap();
    let thread = Uuid::new_v4();
    let sessions = fixture.home.join("sessions/2026/09/23");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join(format!("rollout-2026-09-23T00-00-00-{thread}.jsonl")),
        format!("{}\n", json!({"type":"session_meta","payload":{"id":thread,"cwd":b,"timestamp":"2026-09-23T00:00:00Z"}}))).unwrap();
    // Simulate a native /resume observed in A; no private rollout database writes.
    let (tx, rx) = oneshot::channel();
    app._runs
        .handle
        .sender
        .send(Request::Inspect(Box::new(move |runner| {
            runner.active[0].0.hub.apply(Decoded {
                request_id: Uuid::new_v4(),
                capture_seq: 1,
                received_at: std::time::Instant::now(),
                change: Change::Request {
                    info: RequestInfo {
                        client_request_index: None,
                        requested_model: None,
                        codex_thread_id: Some(thread),
                        codex_turn_id: Some("synthetic-turn".into()),
                        purpose: RequestPurpose::Conversation,
                        purpose_basis: PurposeBasis::CodexTurnMetadata,
                    },
                },
            });
            let _ = tx.send(());
        })))
        .unwrap();
    rx.await.unwrap();
    let error = connect(
        &app.entry,
        Connect {
            project: Some(b),
            resume: Some(thread),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("native_session_running"));
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("invocations"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(
        json_body(get(&app, &cookie, &format!("/workbench/v1/runs/{}", a.run_id)).await).await["state"],
        "running"
    );
}

#[tokio::test]
async fn stopped_run_retention_is_bounded_and_retired_routes_never_select_another_project() {
    let fixture = Fixture::new();
    let app = fixture.app(Overrides {
        codex_bin: Some(fixture.cli()),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let mut first = None;
    for index in 0..MAX_STOPPED_RUNS + 2 {
        let cwd = fixture.dir.path().join(format!("retention-{index}"));
        std::fs::create_dir(&cwd).unwrap();
        let run = connect(
            &app.entry,
            Connect {
                project: Some(cwd),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
        first.get_or_insert(run.run_id);
        assert_eq!(
            launch_post(
                &app,
                &cookie,
                &format!("/workbench/v1/runs/{}/stop", run.run_id),
                json!({"epoch":run.run_id})
            )
            .await
            .status(),
            202
        );
        run_state(&app, &cookie, &run.run_id.to_string(), "stopped").await;
        let home = json_body(get(&app, &cookie, "/workbench/v1/application").await).await;
        assert!(home["runs"].as_array().unwrap().len() <= MAX_STOPPED_RUNS);
    }
    assert_eq!(
        get(
            &app,
            &cookie,
            &format!("/workbench/v1/runs/{}/live/snapshot", first.unwrap())
        )
        .await
        .status(),
        404
    );
    assert_eq!(get(&app, &cookie, "/").await.status(), 200);
}

#[tokio::test]
async fn application_restart_invalidates_run_operation_and_owner_credentials_without_replay() {
    let fixture = Fixture::new();
    let cli = fixture.cli();
    let app = fixture.app(Overrides {
        codex_bin: Some(cli.clone()),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let target = target_for(&app, &cookie, &fixture.cwd).await;
    let body = start_body(&app, &target);
    launch_post(&app, &cookie, "/workbench/v1/runs", body.clone()).await;
    let operation = operation_done(&app, &cookie, &body["operationId"]).await;
    assert_eq!(operation["state"], "ready");
    let old_id = app.instance_id;
    let old_token = reqwest::Url::parse(&app.entry.url)
        .unwrap()
        .fragment()
        .unwrap()
        .strip_prefix("pair=")
        .unwrap()
        .to_owned();
    drop(app);

    let app = fixture.app(Overrides {
        codex_bin: Some(cli),
        ..Default::default()
    });
    assert_ne!(app.instance_id, old_id);
    assert_eq!(
        get(&app, &cookie, "/workbench/v1/application")
            .await
            .status(),
        401
    );
    assert_eq!(
        launch_post(&app, "", "/workbench/v1/pair", json!({"token":old_token}))
            .await
            .status(),
        403
    );
    let cookie = pair(&app).await;
    assert_eq!(
        json_body(get(&app, &cookie, "/workbench/v1/application").await).await["runs"],
        json!([])
    );
    assert_eq!(
        get(
            &app,
            &cookie,
            &format!(
                "/workbench/v1/launch-operations/{}",
                body["operationId"].as_str().unwrap()
            )
        )
        .await
        .status(),
        404
    );
    assert_eq!(
        get(
            &app,
            &cookie,
            &format!(
                "/workbench/v1/runs/{}/live/snapshot",
                operation["run"]["runId"].as_str().unwrap()
            )
        )
        .await
        .status(),
        404
    );
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("invocations"))
            .unwrap()
            .lines()
            .count(),
        2,
        "restart must not probe or start CLI"
    );
}
