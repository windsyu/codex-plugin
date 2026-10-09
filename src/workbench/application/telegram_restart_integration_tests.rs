// Two sequential real host processes, sharing only an isolated synthetic HOME.
// Host Drop and kernel peer/wrapper disappearance are observed independently.
// This does not classify NativeExited versus the production reaper fallback,
// prove crash recovery, or establish that every possible native tool has exited.
const HOST_RESTART_STAGES: &[&str] = &[
    "starting",
    "cold",
    "configure",
    "source",
    "pair",
    "old_prompt",
    "old_complete",
    "old_pending",
    "old_queued",
    "stale",
    "new_pending",
    "new_complete",
    "shutdown",
    "passed",
];
const HOST_RESTART_TOKEN: &str =
    "101:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
fn host_restart_stage(root: &Path, phase: usize, name: &'static str) {
    assert!(HOST_RESTART_STAGES.contains(&name));
    std::fs::write(root.join(format!("stage-{phase}")), name).unwrap();
}
struct HostRestartWire {
    scope: Arc<FileScopeWire>,
    phase: usize,
    inspections: AtomicUsize,
    max_offset: AtomicUsize,
    attempts: AtomicUsize,
    final_gate: RebindFinalGate,
}
async fn host_restart_bot(
    State(wire): State<Arc<HostRestartWire>>,
    uri: Uri,
    body: Bytes,
) -> axum::Json<Value> {
    let request: Value = serde_json::from_slice(&body).unwrap();
    if uri.path().ends_with("/getMe") || uri.path().ends_with("/getWebhookInfo") {
        wire.inspections.fetch_add(1, Ordering::SeqCst);
    } else if uri.path().ends_with("/getUpdates") {
        if let Some(offset) = request["offset"].as_u64() {
            assert!(offset <= 108);
            wire.max_offset.fetch_max(offset as usize, Ordering::SeqCst);
        }
    } else if uri.path().ends_with("/sendMessage") {
        let sent = wire.scope.transport.sent.lock().unwrap().len();
        let text = request["text"].as_str().unwrap();
        match (wire.phase, sent) {
            (1, 0) => file_scope_prompt(text, &wire.scope.cwd, "denied"),
            (1, 1) => assert_eq!(text, "SYNTHETIC_RESTART_OLD_FINAL"),
            (1, 2) => rebind_notice(text),
            (2, 0) => {
                assert_eq!(text, "SYNTHETIC_RESTART_NEW_FINAL");
                assert!(wire.final_gate.open.load(Ordering::SeqCst));
                assert_eq!(wire.attempts.load(Ordering::SeqCst), 1);
            }
            _ => panic!("unexpected restart delivery"),
        }
    }
    file_scope_bot(State(wire.scope.clone()), uri, body).await
}
async fn host_restart_model(State(wire): State<Arc<HostRestartWire>>, body: Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap();
    let transport = &wire.scope.transport;
    let sequence = transport.model_sequence.fetch_add(1, Ordering::SeqCst) + 1;
    assert!(sequence <= 8);
    let title = request["text"]["format"]["type"] == "json_schema"
        && request["text"]["format"]["schema"]["properties"]["title"]["type"] == "string"
        && request["text"]["format"]["schema"]["required"] == json!(["title"]);
    let attempt = if title {
        transport.title_requests.fetch_add(1, Ordering::SeqCst);
        0
    } else {
        let attempt = wire.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(attempt <= if wire.phase == 1 { 3 } else { 1 });
        let input = request["input"].as_array().unwrap();
        let users = |marker: &str| {
            input
                .iter()
                .filter(|item| {
                    item["role"] == "user" && item["content"].to_string().contains(marker)
                })
                .count()
        };
        assert_eq!(users("SYNTHETIC_RESTART_OLD_INPUT"), 1);
        assert_eq!(users("SYNTHETIC_RESTART_QUEUED_INPUT"), 0);
        assert_eq!(
            users("SYNTHETIC_RESTART_NEW_INPUT"),
            usize::from(wire.phase == 2)
        );
        let output = input.iter().find(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "call_restart_denied"
        });
        if wire.phase == 1 && attempt == 1 {
            assert!(output.is_none());
            assert_eq!(users("SYNTHETIC_RESTART_PENDING_INPUT"), 0);
        } else {
            let output =
                output.expect("prior native file denial remains in context")["output"].to_string();
            assert!(output.contains("rejected") || output.contains("declined"));
            assert_eq!(
                users("SYNTHETIC_RESTART_PENDING_INPUT"),
                usize::from(wire.phase == 2 || attempt == 3)
            );
            if wire.phase == 2 || attempt == 3 {
                assert!(input.iter().any(|item| {
                    item["role"] == "assistant"
                        && item["content"]
                            .to_string()
                            .contains("SYNTHETIC_RESTART_OLD_FINAL")
                }));
            }
        }
        file_scope_files(&wire.scope.cwd, "denied", false);
        file_scope_files(&wire.scope.cwd, "allowed", false);
        transport.model.lock().unwrap().push(request);
        attempt
    };
    let stream = async_stream::stream! {
        let event = |value: Value| Bytes::from(format!("data: {value}\n\n"));
        let id = format!("restart_response_{}_{}", wire.phase, sequence);
        yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":id,"model":"gpt-6-astra"}})));
        // First host's second task remains in flight through normal host Drop.
        // Second host's new task is held until the test proves no early final.
        if !title && ((wire.phase == 1 && attempt == 3) || wire.phase == 2) {
            wire.final_gate.wait().await;
        }
        let item = if wire.phase == 1 && attempt == 1 {
            let args = json!({"cmd":file_scope_patch("denied"),"login":false,"yield_time_ms":1000});
            json!({"id":format!("restart_item_{sequence}"),"type":"function_call","call_id":"call_restart_denied","name":"exec_command","arguments":args.to_string()})
        } else {
            let text = if title { r#"{"title":"Synthetic host restart"}"# }
                else if wire.phase == 2 { "SYNTHETIC_RESTART_NEW_FINAL" }
                else if attempt == 2 { "SYNTHETIC_RESTART_OLD_FINAL" }
                else { "SYNTHETIC_RESTART_WITHHELD_FINAL" };
            json!({"id":format!("restart_message_{sequence}"),"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":text}]})
        };
        yield Ok(event(json!({"type":"response.output_item.added","output_index":0,"item":item})));
        yield Ok(event(json!({"type":"response.output_item.done","output_index":0,"item":item})));
        yield Ok(event(json!({"type":"response.completed","response":{"id":id}})));
    };
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}
async fn host_restart_source(app: &Application, cookie: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let value = sources(app, cookie).await;
            if let Some(run) = value["runs"].as_array().and_then(|runs| {
                runs.iter().find(|run| {
                    run["source"]["targets"]
                        .as_array()
                        .is_some_and(|targets| targets.len() == 1)
                })
            }) {
                break run.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("real TUI subscribed source")
}
async fn host_restart_confirm(app: &Application, cookie: &str, wire: &Wire, id: i64) -> Value {
    let pairing = owner(app, cookie, "pair", json!({})).await;
    fresh_poll(wire).await;
    fresh_poll(wire).await;
    wire.updates.lock().unwrap().push_back(update(
        id,
        id,
        202,
        pairing["pairing"]["command"].as_str().unwrap(),
    ));
    let candidate = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let value = status(app, cookie).await;
            if value["channel"]["binding"]["candidate"].is_object() {
                break value["channel"]["binding"]["candidate"].clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let source = host_restart_source(app, cookie).await;
    let confirmed = owner(
        app,
        cookie,
        "confirm",
        json!({"candidate":candidate,
        "runId":source["runId"],"threadId":source["source"]["targets"][0]["threadId"],
        "sourceRevision":source["source"]["revision"],"interactiveConsent":true}),
    )
    .await;
    assert_eq!(
        confirmed["channel"]["binding"]["active"]["interactiveConsent"],
        true
    );
    fresh_poll(wire).await;
    fresh_poll(wire).await;
    confirmed["channel"]["binding"].clone()
}
fn host_restart_inactive(value: &Value) {
    assert!(value["channel"]["binding"]["active"].is_null());
    assert!(value["channel"]["binding"]["candidate"].is_null());
    assert_eq!(value["channel"]["queued"], 0);
    assert!(value["channel"]["lastTask"].is_null());
}
fn host_restart_task(records: &[Value], update: i64) -> Value {
    let tasks: Vec<_> = records
        .iter()
        .filter(|r| r["kind"] == "received" && r["update_id"] == update)
        .collect();
    assert_eq!(tasks.len(), 1);
    tasks[0]["task"].clone()
}
// Read-only reduction of actual receipt rows, never synthetic phase seeding.
fn host_restart_counts(records: &[Value], handoff: &Value) -> Value {
    let mut counts = [0usize; 3];
    for (field, update) in [("completeTask", 2), ("pendingTask", 4), ("queuedTask", 5)] {
        let task = host_restart_task(records, update);
        assert_eq!(task, handoff[field]);
        let submitted = records
            .iter()
            .filter(|r| r["kind"] == "submitting" && r["task"] == task)
            .count();
        let closed = records
            .iter()
            .filter(|r| r["kind"] == "closed" && r["task"] == task)
            .count();
        assert_eq!(submitted, usize::from(field != "queuedTask"));
        assert!(closed <= 1);
        if field == "completeTask" {
            assert_eq!(closed, 1);
        }
        // Normal Drop may close the active actor. The queued body was never
        // submitted; either retained Received or a legitimate Closed is evidence.
        counts[if closed == 1 {
            2
        } else if submitted == 1 {
            1
        } else {
            0
        }] += 1;
    }
    assert_eq!(
        records.iter().filter(|r| r["kind"] == "received").count(),
        3
    );
    assert_eq!(
        records
            .iter()
            .filter(|r| r["kind"] == "callback_received")
            .count(),
        1
    );
    json!({"received":counts[0],"submitting":counts[1],"closed":counts[2]})
}
async fn host_restart_completed(app: &Application, cookie: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let value = status(app, cookie).await;
            let task = &value["channel"]["lastTask"];
            if task["nativeCompleted"] == true && task["delivery"] == "confirmed" {
                assert_eq!(task["nativeConfirmed"], true);
                assert_eq!(task["phase"], "completed");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("accurate native Turn completion and channel delivery");
}
async fn host_restart_guard(root: &Path, child: &mut tokio::process::Child) -> NativeGuard {
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let alias = std::fs::read_dir(root.join("runtime"))
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path().join("app.sock"))
                .find(|path| path.exists());
            if let Some(alias) = alias
                && let Ok(socket) = tokio::net::UnixStream::connect(&alias).await
            {
                use std::os::unix::fs::{FileTypeExt, MetadataExt};
                let credentials = socket.peer_cred().unwrap();
                assert_eq!(credentials.uid(), unsafe { libc::geteuid() });
                let peer = ProcessIdentity::read(credentials.pid().unwrap()).unwrap();
                assert_ne!(peer.group, unsafe { libc::getpgrp() } as u32);
                let wrapper = ProcessIdentity::read(peer.group as i32).unwrap();
                assert_eq!(wrapper.pid as u32, wrapper.group);
                let leaf = alias.canonicalize().unwrap();
                let metadata = std::fs::symlink_metadata(&leaf).unwrap();
                assert!(metadata.file_type().is_socket());
                assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
                break NativeGuard {
                    peer,
                    wrapper,
                    leaf,
                    inode: (metadata.dev(), metadata.ino()),
                };
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "host failed before peer acquisition"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned native peer deadline")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit original CLI 0.156.1; two isolated production hosts with synthetic peers"]
async fn installed_cli_production_telegram_host_restart_path() {
    const CHILD: &str = "CODEX_TELEGRAM_HOST_RESTART_CHILD";
    const TEST: &str = "workbench::application::tests::telegram_native_integration::installed_cli_production_telegram_host_restart_path";
    if std::env::var_os(CHILD).is_none() {
        let root = tempfile::Builder::new()
            .prefix("ctg-")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        for directory in ["user", "native", "runtime", "project"] {
            std::fs::create_dir(root.path().join(directory)).unwrap();
            std::fs::set_permissions(
                root.path().join(directory),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        for phase in [1, 2] {
            let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args([TEST, "--exact", "--ignored", "--nocapture"])
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("HOME", root.path().join("user"))
                .env("USERPROFILE", root.path().join("user"))
                .env("CODEX_HOME", root.path().join("native"))
                .env("TMPDIR", root.path().join("runtime"))
                .env(
                    "CODEX_CHANNEL_TEST_BINARY",
                    std::env::var_os("CODEX_CHANNEL_TEST_BINARY").expect("select original CLI"),
                )
                .env(CHILD, phase.to_string())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let native = host_restart_guard(root.path(), &mut child).await;
            std::fs::write(root.path().join(format!("guarded-{phase}")), b"ready").unwrap();
            let exit = tokio::time::timeout(Duration::from_secs(150), child.wait()).await;
            let raw = std::fs::read_to_string(root.path().join(format!("stage-{phase}")))
                .unwrap_or_default();
            let safe = HOST_RESTART_STAGES
                .iter()
                .copied()
                .find(|stage| *stage == raw)
                .unwrap_or("unknown");
            if let Ok(line) = std::fs::read_to_string(root.path().join(format!("failure-{phase}")))
                && let Ok(line) = line.parse::<u32>()
                && line <= 10000
            {
                println!("TELEGRAM_HOST_RESTART_FAILED phase={phase} fixture_line={line}");
            }
            assert!(
                matches!(exit, Ok(Ok(status)) if status.success()),
                "isolated host phase {phase} failed at {safe}"
            );
            assert!(root.path().join(format!("passed-{phase}")).exists());
            assert!(
                !root.path().join(format!("panic-{phase}")).exists(),
                "any child or HTTP handler panic invalidates acceptance"
            );
            // This precedes guard Drop and phase 2 spawn; cleanup cannot turn
            // a failed lifecycle into successful restart evidence.
            assert!(
                native.peer.gone() && native.wrapper.gone(),
                "exact old native peer and wrapper must be gone before restart"
            );
            drop(native);
            if phase == 1 {
                // All first host workers are gone. Record its final journal,
                // including any legitimate graceful-Drop actor closure.
                let user = root.path().join("user");
                let paths = WorkbenchPaths::from_user_home(&user).unwrap();
                let handoff: Value =
                    serde_json::from_slice(&std::fs::read(root.path().join("handoff")).unwrap())
                        .unwrap();
                let scope = FileScopeWire {
                    transport: Arc::new(Wire::default()),
                    journal: paths.root.join("channel/receipts.jsonl"),
                    acknowledgments: AtomicUsize::new(0),
                    cwd: root.path().join("project").canonicalize().unwrap(),
                };
                let counts = host_restart_counts(&file_scope_receipts(&scope), &handoff);
                std::fs::write(root.path().join("recovered-counts"), counts.to_string()).unwrap();
            }
        }
        println!(
            "TELEGRAM_HOST_RESTART_PRODUCTION original_cli_01561=true separate_hosts=true prior_peer_wrapper_gone=true cold_authority_empty=true preserved_receipts_no_replay=true explicit_local_resume=true fresh_binding=true prior_completed_context=true stale_refs_excluded=true new_native_final_once=true"
        );
        return;
    }
    let phase: usize = std::env::var(CHILD).unwrap().parse().unwrap();
    assert!([1, 2].contains(&phase));
    let user = PathBuf::from(std::env::var_os("HOME").unwrap());
    let home = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
    let root = user.parent().unwrap();
    let failure = root.join(format!("failure-{phase}"));
    let marker = root.join(format!("panic-{phase}"));
    let panicked = Arc::new(AtomicBool::new(false));
    let hook = panicked.clone();
    std::panic::set_hook(Box::new(move |info| {
        hook.store(true, Ordering::SeqCst);
        let _ = std::fs::write(&marker, b"panic-observed");
        if let Some(location) = info.location()
            && location
                .file()
                .ends_with("telegram_restart_integration_tests.rs")
        {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&failure)
            {
                let _ = write!(file, "{}", location.line());
            }
        }
    }));
    host_restart_stage(root, phase, "starting");
    let binary = PathBuf::from(std::env::var_os("CODEX_CHANNEL_TEST_BINARY").unwrap());
    let version = tokio::process::Command::new(&binary)
        .arg("--version")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &user)
        .env("USERPROFILE", &user)
        .env("CODEX_HOME", &home)
        .env("TMPDIR", root.join("runtime"))
        .output()
        .await
        .unwrap();
    assert!(version.status.success());
    assert!(
        String::from_utf8_lossy(&version.stdout).trim() == "codex-cli 0.156.1",
        "unqualified original CLI version"
    );
    let cwd = root.join("project").canonicalize().unwrap();
    let paths = WorkbenchPaths::from_user_home(&user).unwrap();
    let scope = Arc::new(FileScopeWire {
        transport: Arc::new(Wire::default()),
        journal: paths.root.join("channel/receipts.jsonl"),
        acknowledgments: AtomicUsize::new(0),
        cwd: cwd.clone(),
    });
    let wire = Arc::new(HostRestartWire {
        scope,
        phase,
        inspections: AtomicUsize::new(0),
        max_offset: AtomicUsize::new(0),
        attempts: AtomicUsize::new(0),
        final_gate: RebindFinalGate::default(),
    });
    let transport = &wire.scope.transport;
    let (bot_addr, _bot) = server(
        Router::new()
            .route("/{*path}", post(host_restart_bot))
            .with_state(wire.clone()),
    )
    .await;
    let (model_addr, _model) = server(
        Router::new()
            .route("/v1/responses", post(host_restart_model))
            .with_state(wire.clone()),
    )
    .await;
    if phase == 1 {
        file_scope_seed(&cwd);
        std::fs::create_dir(home.join("app-server-daemon")).unwrap();
        std::fs::write(
            home.join("app-server-daemon/settings.json"),
            r#"{"remoteControlEnabled":false,"updater":{"autoUpdateEnabled":false}}"#,
        )
        .unwrap();
    }
    std::fs::write(home.join("config.toml"), format!(
        "model=\"gpt-6-astra\"\nmodel_provider=\"custom\"\napproval_policy=\"on-request\"\napprovals_reviewer=\"user\"\nsandbox_mode=\"read-only\"\ncheck_for_update_on_startup=false\n[features]\ndaemon_auto_start=false\n[model_providers.custom]\nname=\"Synthetic restart integration\"\nbase_url=\"http://{model_addr}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nexperimental_bearer_token=\"synthetic-only\"\n[projects.{}]\ntrust_level=\"trusted\"\n", toml::Value::String(cwd.display().to_string()))).unwrap();
    let prepared = Prepared::load_application(
        &home,
        &cwd,
        &paths,
        None,
        Overrides {
            codex_bin: Some(binary),
            channel_ready: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    let Acquisition::Owner(lock) =
        InstanceLock::acquire(&paths, prepared.config_dir(), &prepared.data_dir).unwrap()
    else {
        panic!("isolated owner required");
    };
    let credential = paths.root.join("channel/telegram.token");
    let before_credential = (phase == 2).then(|| std::fs::read(&credential).unwrap());
    if let Some(bytes) = &before_credential {
        assert!(
            bytes == HOST_RESTART_TOKEN.as_bytes(),
            "actual saved synthetic credential required"
        );
    }
    let before_journal = (phase == 2).then(|| std::fs::read(&wire.scope.journal).unwrap());
    let app = Application::start(prepared, home, paths, lock, None).unwrap();
    app.web.handle().telegram_api_for_test(bot_addr);
    let cookie = pair(&app).await;
    let handoff: Value = if phase == 2 {
        serde_json::from_slice(&std::fs::read(root.join("handoff")).unwrap()).unwrap()
    } else {
        Value::Null
    };
    host_restart_stage(root, phase, "cold");
    let cold = status(&app, &cookie).await;
    host_restart_inactive(&cold);
    assert_eq!(cold["channel"]["configured"], false);
    assert_eq!(
        cold["channel"]["recovered"],
        json!({"received":0,"submitting":0,"closed":0})
    );
    assert_eq!(sources(&app, &cookie).await["runs"], json!([]));
    if phase == 2 {
        assert_ne!(json!(app.instance_id), handoff["instance"]);
        assert_eq!(
            get(
                &app,
                handoff["cookie"].as_str().unwrap(),
                "/workbench/v1/application/telegram"
            )
            .await
            .status(),
            401
        );
        let response = client()
            .post(format!(
                "{}/workbench/v1/application/telegram/pair",
                app.entry.address
            ))
            .header("Cookie", &cookie)
            .header("Origin", &app.entry.address)
            .header("Content-Type", "application/json")
            .body(json!({"instanceId":handoff["instance"]}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 409);
        assert_eq!(json_body(response).await["error"]["code"], "stale_instance");
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(wire.inspections.load(Ordering::SeqCst), 0);
        assert_eq!(transport.polls.load(Ordering::SeqCst), 0);
        assert_eq!(wire.attempts.load(Ordering::SeqCst), 0);
        assert!(transport.sent.lock().unwrap().is_empty());
        assert!(std::fs::read(&credential).unwrap() == *before_credential.as_ref().unwrap());
        assert!(std::fs::read(&wire.scope.journal).unwrap() == *before_journal.as_ref().unwrap());
    }
    host_restart_stage(root, phase, "configure");
    let configured = owner(
        &app,
        &cookie,
        "configure",
        if phase == 1 {
            json!({"token":HOST_RESTART_TOKEN})
        } else {
            json!({})
        },
    )
    .await;
    assert_eq!(configured["channel"]["configured"], true);
    host_restart_inactive(&configured);
    assert_eq!(wire.inspections.load(Ordering::SeqCst), 2);
    if phase == 2 {
        let counts: Value =
            serde_json::from_slice(&std::fs::read(root.join("recovered-counts")).unwrap()).unwrap();
        assert_eq!(configured["channel"]["recovered"], counts);
        assert_eq!(
            host_restart_counts(&file_scope_receipts(&wire.scope), &handoff),
            counts
        );
        assert!(std::fs::read(&credential).unwrap() == *before_credential.as_ref().unwrap());
    }
    host_restart_stage(root, phase, "source");
    // Explicit LOCAL launch/resume. Telegram remains unbound and cannot resume.
    app._runs
        .handle
        .launch(Connect {
            project: Some(cwd.clone()),
            resume: (phase == 2)
                .then(|| uuid::Uuid::parse_str(handoff["thread"].as_str().unwrap()).unwrap()),
            ..Default::default()
        })
        .await
        .unwrap();
    let source = host_restart_source(&app, &cookie).await;
    until(|| root.join(format!("guarded-{phase}")).exists()).await;
    if phase == 2 {
        assert_eq!(
            source["source"]["targets"][0]["threadId"],
            handoff["thread"]
        );
        assert_ne!(source["runId"], handoff["target"]["runEpoch"]);
        host_restart_inactive(&status(&app, &cookie).await);
        assert_eq!(wire.attempts.load(Ordering::SeqCst), 0);
    }
    host_restart_stage(root, phase, "pair");
    let binding =
        host_restart_confirm(&app, &cookie, transport, if phase == 1 { 1 } else { 100 }).await;
    if phase == 1 {
        host_restart_stage(root, phase, "old_prompt");
        transport.updates.lock().unwrap().push_back(update(
            2,
            2,
            202,
            "SYNTHETIC_RESTART_OLD_INPUT",
        ));
        until(|| transport.sent.lock().unwrap().len() == 1).await;
        let prompt = transport.sent.lock().unwrap()[0].clone();
        let reference = prompt["reply_markup"]["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap())
            .find(|button| button["text"] == "Deny")
            .unwrap()["callback_data"]
            .as_str()
            .unwrap()
            .to_owned();
        fresh_poll(transport).await;
        fresh_poll(transport).await;
        transport
            .updates
            .lock()
            .unwrap()
            .push_back(interaction_callback(
                3,
                202,
                1,
                "callback_file_restart_first",
                &reference,
            ));
        until(|| transport.sent.lock().unwrap().len() == 2).await;
        host_restart_completed(&app, &cookie).await;
        host_restart_stage(root, phase, "old_complete");
        // Confirmed delivery is published before the executor's receipt close.
        // Observe that actual close and polling progress before admitting the
        // next task, so this fixture does not deliberately exercise that race.
        let complete_task = host_restart_task(&file_scope_receipts(&wire.scope), 2);
        until(|| {
            file_scope_receipts(&wire.scope)
                .iter()
                .any(|row| row["kind"] == "closed" && row["task"] == complete_task)
        })
        .await;
        fresh_poll(transport).await;
        fresh_poll(transport).await;
        assert_eq!(wire.attempts.load(Ordering::SeqCst), 2);
        assert_eq!(wire.scope.acknowledgments.load(Ordering::SeqCst), 1);
        file_scope_files(&cwd, "denied", false);
        host_restart_stage(root, phase, "old_pending");
        transport.updates.lock().unwrap().push_back(update(
            4,
            4,
            202,
            "SYNTHETIC_RESTART_PENDING_INPUT",
        ));
        until(|| wire.attempts.load(Ordering::SeqCst) == 3).await;
        assert_eq!(
            status(&app, &cookie).await["channel"]["lastTask"]["nativeCompleted"],
            false
        );
        host_restart_stage(root, phase, "old_queued");
        transport.updates.lock().unwrap().push_back(update(
            5,
            5,
            202,
            "SYNTHETIC_RESTART_QUEUED_INPUT",
        ));
        until(|| transport.sent.lock().unwrap().len() == 3).await;
        fresh_poll(transport).await;
        fresh_poll(transport).await;
        assert_eq!(status(&app, &cookie).await["channel"]["queued"], 1);
        let notice = transport.sent.lock().unwrap()[2]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        rebind_notice(&notice);
        let cancel = notice
            .split("/cancel ")
            .nth(1)
            .unwrap()
            .lines()
            .next()
            .unwrap();
        let records = file_scope_receipts(&wire.scope);
        let handoff = json!({"cookie":cookie,"instance":app.instance_id,
            "thread":source["source"]["targets"][0]["threadId"],"generation":binding["generation"],
            "target":binding["active"]["target"],"callback":reference,"cancel":cancel,
            "completeTask":host_restart_task(&records,2),"pendingTask":host_restart_task(&records,4),"queuedTask":host_restart_task(&records,5)});
        host_restart_counts(&records, &handoff);
        assert!(
            !records
                .iter()
                .any(|row| row["kind"] == "closed" && row["task"] == handoff["pendingTask"])
        );
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(root.join("handoff"))
            .unwrap();
        file.write_all(&serde_json::to_vec(&handoff).unwrap())
            .unwrap();
        file.sync_all().unwrap();
    } else {
        assert_ne!(binding["generation"], handoff["generation"]);
        for field in ["runEpoch", "transportEpoch", "subscriptionGeneration"] {
            for target in [&binding["active"]["target"], &handoff["target"]] {
                assert!(target[field].as_str().is_some_and(|value| {
                    uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil())
                }));
            }
            assert_ne!(binding["active"]["target"][field], handoff["target"][field]);
        }
        assert_eq!(binding["active"]["target"]["threadId"], handoff["thread"]);
        host_restart_stage(root, phase, "stale");
        let old_rows = file_scope_receipts(&wire.scope);
        transport.updates.lock().unwrap().extend([
            update(101, 2, 202, "SYNTHETIC_RESTART_OLD_INPUT"),
            update(102, 4, 202, "SYNTHETIC_RESTART_PENDING_INPUT"),
            update(103, 5, 202, "SYNTHETIC_RESTART_QUEUED_INPUT"),
            interaction_callback(
                104,
                202,
                1,
                "callback_file_restart_first",
                handoff["callback"].as_str().unwrap(),
            ),
            interaction_callback(
                105,
                202,
                1,
                "callback_file_restart_stale",
                handoff["callback"].as_str().unwrap(),
            ),
            update(
                106,
                106,
                202,
                &format!("/cancel {}", handoff["cancel"].as_str().unwrap()),
            ),
        ]);
        until(|| wire.max_offset.load(Ordering::SeqCst) >= 107).await;
        let stale_rows = file_scope_receipts(&wire.scope);
        assert_eq!(stale_rows.len(), old_rows.len() + 2);
        for id in [105, 106] {
            assert_eq!(
                stale_rows
                    .iter()
                    .filter(|row| row["kind"] == "rejected" && row["update_id"] == id)
                    .count(),
                1
            );
        }
        assert_eq!(&stale_rows[..old_rows.len()], old_rows.as_slice());
        assert_eq!(wire.attempts.load(Ordering::SeqCst), 0);
        assert_eq!(wire.scope.acknowledgments.load(Ordering::SeqCst), 0);
        assert!(transport.sent.lock().unwrap().is_empty());
        assert_eq!(status(&app, &cookie).await["channel"]["queued"], 0);
        assert!(status(&app, &cookie).await["channel"]["lastTask"].is_null());
        host_restart_stage(root, phase, "new_pending");
        transport.updates.lock().unwrap().push_back(update(
            107,
            107,
            202,
            "SYNTHETIC_RESTART_NEW_INPUT",
        ));
        until(|| wire.attempts.load(Ordering::SeqCst) == 1).await;
        assert!(transport.sent.lock().unwrap().is_empty());
        assert_eq!(
            status(&app, &cookie).await["channel"]["lastTask"]["nativeCompleted"],
            false
        );
        let pending = file_scope_receipts(&wire.scope);
        let new_task = host_restart_task(&pending, 107);
        assert_eq!(
            pending
                .iter()
                .filter(|row| row["kind"] == "submitting" && row["task"] == new_task)
                .count(),
            1
        );
        assert!(
            !pending
                .iter()
                .any(|row| row["kind"] == "closed" && row["task"] == new_task)
        );
        wire.final_gate.release();
        host_restart_completed(&app, &cookie).await;
        host_restart_stage(root, phase, "new_complete");
        fresh_poll(transport).await;
        fresh_poll(transport).await;
        assert_eq!(transport.sent.lock().unwrap().len(), 1);
        assert_eq!(wire.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(wire.scope.acknowledgments.load(Ordering::SeqCst), 0);
        let records = file_scope_receipts(&wire.scope);
        assert_eq!(&records[..stale_rows.len()], stale_rows.as_slice());
        assert_eq!(records.len(), stale_rows.len() + 3);
        assert_eq!(
            records
                .iter()
                .filter(|row| row["kind"] == "closed" && row["task"] == new_task)
                .count(),
            1
        );
        assert_eq!(
            status(&app, &cookie).await["channel"]["lastTask"]["id"],
            new_task
        );
        assert_eq!(
            host_restart_source(&app, &cookie).await["source"]["targets"][0]["threadId"],
            handoff["thread"]
        );
        file_scope_files(&cwd, "denied", false);
    }
    host_restart_stage(root, phase, "shutdown");
    drop(app);
    assert!(!panicked.load(Ordering::SeqCst));
    host_restart_stage(root, phase, "passed");
    std::fs::write(root.join(format!("passed-{phase}")), b"passed").unwrap();
    // Real host return semantics: no detached Rust worker remains alive to
    // append after the parent takes the phase-1 final receipt snapshot.
    std::process::exit(0);
}
