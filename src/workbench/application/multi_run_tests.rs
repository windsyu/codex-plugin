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

#[tokio::test]
async fn concurrent_capacity_rejection_preserves_input_and_stop_isolation_without_replay() {
    let fixture = Fixture::new();
    let cli = fixture.cli();
    // Isolate even the synthetic child environment; each project records exactly
    // the bytes consumed by its own PTY, rather than trusting an input ACK.
    let script = std::fs::read_to_string(&cli).unwrap();
    let script = script
        .replace(
            "#!/bin/sh\n",
            "#!/bin/sh\nexport HOME=\"$CODEX_HOME\" USERPROFILE=\"$CODEX_HOME\"\n",
        )
        .replace(
            "exec /bin/cat",
            "stty -echo\nwhile IFS= read -r line; do printf '%s\\n' \"$line\" >> inputs.txt; done",
        );
    std::fs::write(&cli, script).unwrap();
    let app = fixture.app(Overrides {
        codex_bin: Some(cli),
        ..Default::default()
    });
    let cookie = pair(&app).await;
    let mut runs = Vec::new();
    for index in 0..MAX_RUNNING_RUNS - 1 {
        let cwd = fixture.dir.path().join(format!("existing-{index}"));
        std::fs::create_dir(&cwd).unwrap();
        let run = connect(
            &app.entry,
            Connect {
                project: Some(cwd.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
        runs.push((run.run_id, cwd));
    }
    let mut requests = Vec::new();
    for index in 0..4 {
        let cwd = fixture.dir.path().join(format!("contender-{index}"));
        std::fs::create_dir(&cwd).unwrap();
        let target = target_for(&app, &cookie, &cwd).await;
        requests.push((start_body(&app, &target), cwd));
    }
    let responses = futures_util::future::join_all(
        requests
            .iter()
            .map(|(body, _)| launch_post(&app, &cookie, "/workbench/v1/runs", body.clone())),
    )
    .await;
    let mut admitted = Vec::new();
    let mut busy = Vec::new();
    for (response, request) in responses.into_iter().zip(&requests) {
        if response.status() == 202 {
            admitted.push(request);
        } else {
            assert_eq!(response.status(), 503);
            assert_eq!(json_body(response).await["error"]["code"], "launch_busy");
            busy.push(request);
        }
    }
    assert!(!admitted.is_empty());
    let mut rejected = Vec::new();
    for (body, cwd) in admitted {
        let operation = operation_done(&app, &cookie, &body["operationId"]).await;
        if operation["state"] == "ready" {
            runs.push((
                serde_json::from_value(operation["run"]["runId"].clone()).unwrap(),
                cwd.clone(),
            ));
        } else {
            assert_eq!(operation["state"], "failed", "{operation}");
            assert_eq!(operation["error"]["code"], "run_capacity");
            rejected.push((body.clone(), cwd.clone()));
        }
    }
    // The bounded start queue can reject admission as launch_busy. Explicit
    // retries after admitted operations settle must report capacity, not spawn.
    for (body, cwd) in busy {
        assert_eq!(
            launch_post(&app, &cookie, "/workbench/v1/runs", body.clone())
                .await
                .status(),
            202
        );
        let operation = operation_done(&app, &cookie, &body["operationId"]).await;
        assert_eq!(operation["state"], "failed");
        assert_eq!(operation["error"]["code"], "run_capacity");
        rejected.push((body.clone(), cwd.clone()));
    }
    assert_eq!(runs.len(), MAX_RUNNING_RUNS);
    assert_eq!(rejected.len(), 3);
    let invocation_count = || {
        std::fs::read_to_string(fixture.home.join("invocations"))
            .unwrap()
            .lines()
            .count()
    };
    assert_eq!(
        invocation_count(),
        MAX_RUNNING_RUNS * 2,
        "capacity rejection must not probe or spawn"
    );

    let (tx, rx) = oneshot::channel();
    app._runs
        .handle
        .sender
        .send(Request::Inspect(Box::new(move |runner| {
            let handles: Vec<_> = runner
                .active
                .iter()
                .map(|(run, _, summary)| (summary.run_id, run.terminal()))
                .collect();
            assert!(tx.send(handles).is_ok());
        })))
        .unwrap();
    let handles = rx.await.unwrap();
    let mut owners = Vec::new();
    for (id, handle) in handles {
        let page = handle.attach().await.unwrap();
        let grant = handle.claim(page.connection_id).await.unwrap();
        handle
            .input(
                page.connection_id,
                grant.generation,
                1,
                format!("first-{id}\n").into_bytes(),
            )
            .await
            .unwrap();
        owners.push((id, handle, page, grant.generation));
    }
    async fn consumed(path: &Path, expected: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if std::fs::read_to_string(path).is_ok_and(|text| text == expected) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the selected CLI must consume exactly its own input");
    }
    for (id, cwd) in &runs {
        consumed(&cwd.join("inputs.txt"), &format!("first-{id}\n")).await;
    }
    let stopped = runs[0].0;
    assert_eq!(
        launch_post(
            &app,
            &cookie,
            &format!("/workbench/v1/runs/{stopped}/stop"),
            json!({"epoch":stopped})
        )
        .await
        .status(),
        202
    );
    run_state(&app, &cookie, &stopped.to_string(), "stopped").await;
    // Reposting a failed operation after capacity is freed must retain the
    // failure. Only a fresh explicit operation may occupy the released slot.
    let (failed_body, replacement_cwd) = &rejected[0];
    launch_post(&app, &cookie, "/workbench/v1/runs", failed_body.clone()).await;
    let failed = operation_done(&app, &cookie, &failed_body["operationId"]).await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["error"]["code"], "run_capacity");
    assert_eq!(invocation_count(), MAX_RUNNING_RUNS * 2);
    let target = target_for(&app, &cookie, replacement_cwd).await;
    let body = start_body(&app, &target);
    assert_eq!(
        launch_post(&app, &cookie, "/workbench/v1/runs", body.clone())
            .await
            .status(),
        202
    );
    assert_eq!(
        operation_done(&app, &cookie, &body["operationId"]).await["state"],
        "ready"
    );
    for (id, handle, page, generation) in owners {
        let result = handle
            .input(
                page.connection_id,
                generation,
                2,
                format!("second-{id}\n").into_bytes(),
            )
            .await;
        if id == stopped {
            assert!(result.is_err());
        } else {
            result.unwrap();
        }
    }
    for (id, cwd) in &runs {
        let expected = if *id == stopped {
            format!("first-{id}\n")
        } else {
            format!("first-{id}\nsecond-{id}\n")
        };
        consumed(&cwd.join("inputs.txt"), &expected).await;
    }
    for (_, cwd) in &rejected {
        assert!(
            !cwd.join("inputs.txt").exists(),
            "new or rejected Run must not inherit prior input"
        );
    }
    assert_eq!(invocation_count(), (MAX_RUNNING_RUNS + 1) * 2);
    let application = json_body(get(&app, &cookie, "/workbench/v1/application").await).await;
    assert_eq!(
        application["runs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|run| run["state"] == "running")
            .count(),
        MAX_RUNNING_RUNS
    );
    assert!(app.healthy());
}

#[tokio::test]
async fn paired_device_cookie_cannot_authorize_local_application_or_mutate_settings() {
    let fixture = Fixture::new();
    let app = fixture.app(Overrides {
        codex_bin: Some(fixture.cli()),
        ..Default::default()
    });
    let owner = pair(&app).await;
    let target = target_for(&app, &owner, &fixture.cwd).await;
    let body = start_body(&app, &target);
    assert_eq!(
        launch_post(&app, &owner, "/workbench/v1/runs", body.clone())
            .await
            .status(),
        202
    );
    let operation = operation_done(&app, &owner, &body["operationId"]).await;
    assert_eq!(operation["state"], "ready");
    let id: Uuid = serde_json::from_value(operation["run"]["runId"].clone()).unwrap();
    let (tx, rx) = oneshot::channel();
    app._runs
        .handle
        .sender
        .send(Request::Inspect(Box::new(move |runner| {
            runner
                .active
                .iter()
                .find(|(_, _, summary)| summary.run_id == id)
                .unwrap()
                .1
                .loopback_device_discovery_for_test();
            tx.send(()).unwrap();
        })))
        .unwrap();
    rx.await.unwrap();
    let run_path = format!("/workbench/v1/runs/{id}");
    let access = json_body(get(&app, &owner, &format!("{run_path}/access")).await).await;
    let enabled = client()
        .post(format!("{}{run_path}/access/enable", app.entry.address))
        .header("Cookie", &owner)
        .header("Origin", &app.entry.address)
        .header("If-Match", format!("\"{}\"", access["revision"]))
        .header("Content-Type", "application/json")
        .body(json!({"runEpoch":id}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(enabled.status(), 202);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = json_body(get(&app, &owner, &format!("{run_path}/access")).await).await;
            if status["state"] == "ready" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("device listener must become ready");
    let invitation =
        json_body(get(&app, &owner, &format!("{run_path}/access/pairing")).await).await;
    let url = reqwest::Url::parse(invitation["links"][0]["url"].as_str().unwrap()).unwrap();
    let device_base = url.origin().ascii_serialization();
    let token = url.fragment().unwrap().strip_prefix("pair=").unwrap();
    let paired = client()
        .post(format!("{device_base}{run_path}/pair"))
        .header("Origin", &device_base)
        .header("Content-Type", "application/json")
        .body(json!({"token":token}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(paired.status(), 204);
    let device = paired.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(
        client()
            .get(format!("{device_base}{run_path}/run"))
            .header("Cookie", &device)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let config_path = fixture.paths.config_dir().join("config.json");
    let config_before = std::fs::read(&config_path).unwrap();
    let invocations = fixture.home.join("invocations");
    let invocations_before = std::fs::read(&invocations).unwrap();
    let before = json_body(get(&app, &owner, "/workbench/v1/application").await).await;
    assert_eq!(before["runs"].as_array().unwrap().len(), 1);
    for (method, path, expected) in [
        (reqwest::Method::GET, "/workbench/v1/application", 401),
        (
            reqwest::Method::GET,
            "/workbench/v1/application/telegram",
            401,
        ),
        (
            reqwest::Method::POST,
            "/workbench/v1/application/telegram/configure",
            401,
        ),
        (
            reqwest::Method::POST,
            "/workbench/v1/application/telegram/pair",
            401,
        ),
        (
            reqwest::Method::POST,
            "/workbench/v1/application/telegram/confirm",
            401,
        ),
        (
            reqwest::Method::POST,
            "/workbench/v1/application/telegram/revoke",
            401,
        ),
        (
            reqwest::Method::POST,
            "/workbench/v1/application/telegram/forget",
            401,
        ),
        (
            reqwest::Method::GET,
            "/workbench/v1/application/channel-runs",
            401,
        ),
        (
            reqwest::Method::GET,
            "/workbench/v1/application/settings",
            401,
        ),
        (reqwest::Method::GET, "/workbench/v1/library/entries", 401),
        (reqwest::Method::GET, "/workbench/v1/library/sources", 401),
        (reqwest::Method::POST, "/workbench/v1/launch-targets", 401),
        (reqwest::Method::POST, "/workbench/v1/runs", 401),
        (
            reqwest::Method::POST,
            "/workbench/v1/application/pick-directory",
            401,
        ),
        (
            reqwest::Method::PUT,
            "/workbench/v1/application/settings",
            401,
        ),
        (reqwest::Method::POST, "/workbench/v1/library/preview", 403),
        (reqwest::Method::POST, "/workbench/v1/library/refresh", 403),
    ] {
        let response = client()
            .request(method, format!("{}{path}", app.entry.address))
            .header("Cookie", &device)
            .header("Origin", &app.entry.address)
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected, "{path}");
    }
    let after = json_body(get(&app, &owner, "/workbench/v1/application").await).await;
    assert_eq!(after["runs"], before["runs"]);
    assert_eq!(std::fs::read(&config_path).unwrap(), config_before);
    assert_eq!(std::fs::read(&invocations).unwrap(), invocations_before);
    assert!(
        !fixture
            .paths
            .config_dir()
            .join("config.previous.json")
            .exists()
    );
}

#[tokio::test]
async fn same_home_shutdown_gate_checks_every_retired_run_before_real_launch() {
    use crate::workbench::launch::{ShutdownHandle, ShutdownOutcome};
    for (first, blocked) in [
        (Some(ShutdownOutcome::NativeExited), None),
        (
            Some(ShutdownOutcome::NativeExited),
            Some(ShutdownOutcome::ForcedUnknown),
        ),
        (
            Some(ShutdownOutcome::NativeExited),
            Some(ShutdownOutcome::NativeExited),
        ),
        (None, Some(ShutdownOutcome::ForcedUnknown)),
    ] {
        let fixture = Fixture::new();
        let cli = fixture.cli();
        let app = fixture.app(Overrides {
            codex_bin: Some(cli),
            ..Default::default()
        });
        let cookie = pair(&app).await;
        let mut ids = Vec::new();
        for index in 0..2 {
            let cwd = fixture.dir.path().join(format!("former-run-{index}"));
            std::fs::create_dir(&cwd).unwrap();
            let target = target_for(&app, &cookie, &cwd).await;
            let body = start_body(&app, &target);
            launch_post(&app, &cookie, "/workbench/v1/runs", body.clone()).await;
            let operation = operation_done(&app, &cookie, &body["operationId"]).await;
            let id = Uuid::parse_str(operation["run"]["runId"].as_str().unwrap()).unwrap();
            ids.push(id);
            launch_post(
                &app,
                &cookie,
                &format!("/workbench/v1/runs/{id}/stop"),
                json!({"epoch":id}),
            )
            .await;
            run_state(&app, &cookie, &id.to_string(), "stopped").await;
        }
        ids.sort();
        let home = fixture.home.canonicalize().unwrap();
        let (sent, received) = tokio::sync::oneshot::channel();
        app._runs
            .handle
            .sender
            .send(Request::Inspect(Box::new(move |runner| {
                // Exercise the real Runner launch path with two retained native
                // lifecycle receipts, independently of process-shutdown timing.
                runner.channel_shutdowns.insert(
                    home,
                    std::collections::BTreeMap::from([
                        (ids[0], ShutdownHandle::test_outcome(first)),
                        (ids[1], ShutdownHandle::test_outcome(blocked)),
                    ]),
                );
                sent.send(()).unwrap();
            })))
            .unwrap();
        received.await.unwrap();
        let before = std::fs::read_to_string(fixture.home.join("invocations"))
            .unwrap()
            .lines()
            .count();
        let target = target_for(&app, &cookie, &fixture.cwd).await;
        let body = start_body(&app, &target);
        launch_post(&app, &cookie, "/workbench/v1/runs", body.clone()).await;
        let result = operation_done(&app, &cookie, &body["operationId"]).await;
        match blocked {
            None => assert_eq!(result["error"]["code"], "channel_shutdown_pending"),
            Some(ShutdownOutcome::ForcedUnknown) => {
                assert_eq!(result["error"]["code"], "channel_shutdown_incomplete")
            }
            Some(ShutdownOutcome::NativeExited) => assert_eq!(result["state"], "ready"),
        }
        let after = std::fs::read_to_string(fixture.home.join("invocations"))
            .unwrap()
            .lines()
            .count();
        assert_eq!(
            after - before,
            if blocked == Some(ShutdownOutcome::NativeExited) {
                2
            } else {
                0
            }
        );
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "requires explicit original CLI; isolated host-process exit regression"]
async fn installed_cli_application_exit_waits_for_native_reaper() {
    use std::os::unix::process::CommandExt;
    const CHILD: &str = "CODEX_CHANNEL_EXIT_CHILD";
    const TEST: &str =
        "workbench::application::tests::installed_cli_application_exit_waits_for_native_reaper";
    if std::env::var_os(CHILD).is_some() {
        let user = PathBuf::from(std::env::var_os("HOME").unwrap());
        let home = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
        let root = user.parent().unwrap();
        let cwd = root.join("project");
        let paths = WorkbenchPaths::from_user_home(&user).unwrap();
        let prepared = Prepared::load_application(
            &home,
            &cwd,
            &paths,
            None,
            Overrides {
                codex_bin: Some(PathBuf::from(
                    std::env::var_os("CODEX_CHANNEL_TEST_BINARY").unwrap(),
                )),
                channel_ready: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        let Acquisition::Owner(lock) =
            InstanceLock::acquire(&paths, prepared.config_dir(), &prepared.data_dir).unwrap()
        else {
            panic!("isolated owner expected")
        };
        let app = Application::start(prepared, home, paths, lock, None).unwrap();
        app._runs
            .handle
            .launch(Connect {
                project: Some(cwd),
                ..Default::default()
            })
            .await
            .unwrap();
        let cookie = pair(&app).await;
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let sources =
                    json_body(get(&app, &cookie, "/workbench/v1/application/channel-runs").await)
                        .await;
                if sources["runs"].as_array().is_some_and(|runs| {
                    runs.iter().any(|run| {
                        run["source"]["targets"]
                            .as_array()
                            .is_some_and(|targets| !targets.is_empty())
                    })
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("original TUI must establish a native subscription");
        std::fs::write(root.join("ready"), b"ready").unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !root.join("exit").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let mut pending = None;
        if std::env::var("CODEX_CHANNEL_EXIT_FORCED").as_deref() == Ok("1") {
            let (send, receive) = oneshot::channel();
            app._runs
                .handle
                .sender
                .send(Request::Inspect(Box::new(move |runner| {
                    assert!(send.send(runner.active[0].0.terminal()).is_ok());
                })))
                .unwrap();
            let terminal = receive.await.unwrap();
            terminal.stop().await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !terminal.exited() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let (send, receive) = oneshot::channel();
            app._runs
                .handle
                .sender
                .send(Request::Inspect(Box::new(move |runner| {
                    runner.collect_channel_shutdowns();
                    assert!(
                        send.send(
                            runner
                                .channel_shutdowns
                                .values()
                                .flat_map(|runs| runs.values())
                                .next()
                                .unwrap()
                                .clone(),
                        )
                        .is_ok()
                    );
                })))
                .unwrap();
            let handle = receive.await.unwrap();
            assert!(
                !handle.is_finished(),
                "forced native cleanup must be pending at host exit"
            );
            pending = Some(handle);
        }
        std::fs::write(root.join("pending-witness"), b"ready").unwrap();
        drop(app);
        if pending.as_ref().is_none_or(|handle| handle.is_finished()) {
            std::fs::write(root.join("reaped-before-exit"), b"ready").unwrap();
        }
        // Model a real binary returning from main: detached Rust threads cannot
        // keep this host alive once the application lifecycle returns.
        std::process::exit(0);
    }
    struct Identity {
        pid: i32,
        started: (u64, u64),
        group: u32,
    }
    impl Identity {
        fn read(pid: i32) -> Option<Self> {
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of_val(&info) as i32;
            let count = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut info as *mut libc::proc_bsdinfo).cast(),
                    size,
                )
            };
            (count == size && info.pbi_uid == unsafe { libc::geteuid() }).then_some(Self {
                pid,
                started: (info.pbi_start_tvsec, info.pbi_start_tvusec),
                group: info.pbi_pgid,
            })
        }
        fn same(&self) -> bool {
            Self::read(self.pid)
                .is_some_and(|other| other.started == self.started && other.group == self.group)
        }
        fn gone(&self) -> bool {
            if let Some(other) = Self::read(self.pid) {
                return other.started != self.started || other.group != self.group;
            }
            (unsafe { libc::kill(self.pid, 0) }) == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        }
        fn cleanup(&self) {
            if self.same() {
                unsafe {
                    libc::kill(self.pid, libc::SIGKILL);
                }
            }
        }
    }
    struct IdentityGuard(Identity);
    impl std::ops::Deref for IdentityGuard {
        type Target = Identity;
        fn deref(&self) -> &Identity {
            &self.0
        }
    }
    impl Drop for IdentityGuard {
        fn drop(&mut self) {
            self.0.cleanup();
        }
    }
    let mut all_ok = true;
    for forced in [false, true] {
        let root = tempfile::Builder::new()
            .prefix("cexit-")
            .tempdir_in("/tmp")
            .unwrap();
        let user = root.path().join("user");
        let home = root.path().join("native");
        let runtime_dir = root.path().join("runtime");
        let cwd = root.path().join("project");
        for path in [&user, &home, &runtime_dir, &cwd] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::create_dir(home.join("app-server-daemon")).unwrap();
        std::fs::write(
            home.join("app-server-daemon/settings.json"),
            r#"{"remoteControlEnabled":false,"updater":{"autoUpdateEnabled":false}}"#,
        )
        .unwrap();
        std::fs::write(home.join("config.toml"),format!("model=\"synthetic-model\"\nmodel_provider=\"custom\"\ncheck_for_update_on_startup=false\n[features]\ndaemon_auto_start=false\n[model_providers.custom]\nname=\"Synthetic exit fixture\"\nbase_url=\"http://127.0.0.1:9/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nexperimental_bearer_token=\"synthetic-only\"\n[projects.{}]\ntrust_level=\"trusted\"\n",toml::Value::String(cwd.display().to_string()))).unwrap();
        let mut sentinel = ChildGuard(
            std::process::Command::new("/bin/sleep")
                .arg("60")
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([TEST, "--exact", "--ignored", "--nocapture"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &user)
            .env("USERPROFILE", &user)
            .env("CODEX_HOME", &home)
            .env("TMPDIR", &runtime_dir)
            .env(CHILD, "1")
            .env("CODEX_CHANNEL_EXIT_FORCED", if forced { "1" } else { "" })
            .env(
                "CODEX_CHANNEL_TEST_BINARY",
                std::env::var_os("CODEX_CHANNEL_TEST_BINARY").expect("select original CLI"),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            while !root.path().join("ready").exists() {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "isolated host startup failed"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("isolated application must launch");
        let alias = std::fs::read_dir(&runtime_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("app.sock"))
            .find(|path| path.exists())
            .expect("owned runtime alias");
        let socket = tokio::net::UnixStream::connect(alias).await.unwrap();
        let cred = socket.peer_cred().unwrap();
        assert_eq!(cred.uid(), unsafe { libc::geteuid() });
        let peer = IdentityGuard(Identity::read(cred.pid().unwrap()).unwrap());
        let wrapper = IdentityGuard(Identity::read(peer.group as i32).unwrap());
        assert_ne!(peer.group, unsafe { libc::getpgrp() } as u32);
        if forced {
            assert!(peer.same());
            assert_eq!(unsafe { libc::kill(peer.pid, libc::SIGSTOP) }, 0);
        }
        drop(socket);
        let began = std::time::Instant::now();
        std::fs::write(root.path().join("exit"), b"exit").unwrap();
        let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
            .await
            .expect("host shutdown bounded")
            .unwrap();
        let host_success = status.success();
        let pending_witness = root.path().join("pending-witness").exists();
        let reaped_before_exit = root.path().join("reaped-before-exit").exists();
        let host_waited = !forced || began.elapsed() >= Duration::from_secs(14);
        let ended = tokio::time::timeout(Duration::from_secs(2), async {
            while !peer.gone() || !wrapper.gone() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok();
        let unrelated_alive = sentinel.0.try_wait().unwrap().is_none();
        all_ok &= host_success
            && pending_witness
            && reaped_before_exit
            && host_waited
            && ended
            && unrelated_alive;
        peer.cleanup();
        wrapper.cleanup();
        let cleaned = tokio::time::timeout(Duration::from_secs(3), async {
            while !peer.gone() || !wrapper.gone() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok();
        println!(
            "APPLICATION_EXIT forced={forced} host_success={host_success} pending_witness={pending_witness} reaped_before_exit={reaped_before_exit} host_waited={host_waited} owned_primary_exited={ended} unrelated_alive={unrelated_alive} exact_cleanup={cleaned}"
        );
        assert!(cleaned && unrelated_alive);
    }
    assert!(
        all_ok,
        "application host exited before its native reapers completed"
    );
}
