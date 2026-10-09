// Included after telegram_file_integration_tests and telegram_tui_interaction_integration_tests.
// Only the channel control transport is artificially backpressured. The original
// TUI, relay, native observer and CLI continue normally. This proves an admitted
// late answer overlapping real TUI handling, not two native pending consumers.
const TUI_INFLIGHT_STAGES: &[&str] = &[
    "starting",
    "source",
    "configure",
    "pair",
    "confirm",
    "local_menu",
    "callback_blocked",
    "tui_answer",
    "release",
    "duplicate",
    "final",
    "shutdown",
    "passed",
];
fn tui_inflight_stage(root: &Path, name: &'static str) {
    assert!(TUI_INFLIGHT_STAGES.contains(&name));
    std::fs::write(root.join("stage"), name).unwrap();
}
async fn tui_inflight_model(State(wire): State<Arc<FileScopeWire>>, body: Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap();
    let transport = &wire.transport;
    let sequence = transport.model_sequence.fetch_add(1, Ordering::SeqCst) + 1;
    assert!(sequence <= 8);
    let title = request["text"]["format"]["type"] == "json_schema"
        && request["text"]["format"]["schema"]["properties"]["title"]["type"] == "string"
        && request["text"]["format"]["schema"]["required"] == json!(["title"]);
    let tool = if title {
        transport.title_requests.fetch_add(1, Ordering::SeqCst);
        false
    } else {
        let input = request["input"].as_array().unwrap();
        assert!(input.iter().any(|item| {
            item["role"] == "user"
                && item["content"]
                    .to_string()
                    .contains("SYNTHETIC_INFLIGHT_INPUT")
        }));
        let output = input.iter().find(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "call_file_scope_1"
        });
        if let Some(output) = output {
            assert_eq!(
                file_scope_receipts(&wire)
                    .iter()
                    .filter(|r| r["kind"] == "callback_received")
                    .count(),
                1
            );
            let text = output["output"].to_string();
            assert!(
                text.contains("Success")
                    && !text.contains("rejected")
                    && !text.contains("declined"),
                "actual TUI approval must produce successful native patch result"
            );
            file_scope_files(&wire.cwd, "allowed", true);
            file_scope_files(&wire.cwd, "denied", false);
        }
        let tool = output.is_none();
        let mut requests = transport.model.lock().unwrap();
        assert!(
            requests.len() < 2,
            "native tool/model continuation must occur once"
        );
        requests.push(request);
        tool
    };
    let stream = async_stream::stream! {
        let event = |value: Value| Bytes::from(format!("data: {value}\n\n"));
        let id = format!("file_scope_response_{sequence}");
        yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":id,"model":"gpt-6-astra"}})));
        if !tool && !title {
            while !wire.transport.permit_final.load(Ordering::SeqCst) { tokio::time::sleep(Duration::from_millis(10)).await; }
        }
        let item = if tool {
            let args = json!({"cmd":file_scope_patch("allowed"),"login":false,"yield_time_ms":1000});
            json!({"id":format!("item_file_{sequence}"),"type":"function_call","call_id":"call_file_scope_1","name":"exec_command","arguments":args.to_string()})
        } else {
            let text = if title { r#"{"title":"Synthetic in-flight answer"}"# } else { "SYNTHETIC_INFLIGHT_FINAL" };
            json!({"id":format!("message_file_{sequence}"),"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":text}]})
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
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit original CLI 0.156.1; real TUI with admitted channel answer under test transport backpressure"]
async fn installed_cli_production_telegram_inflight_answer_path() {
    const CHILD: &str = "CODEX_TELEGRAM_TUI_INFLIGHT_CHILD";
    const TEST: &str = "workbench::application::tests::telegram_native_integration::installed_cli_production_telegram_inflight_answer_path";
    if std::env::var_os(CHILD).is_none() {
        let root = tempfile::Builder::new()
            .prefix("ctg-")
            .tempdir_in("/tmp")
            .unwrap();
        for directory in ["user", "native", "runtime", "project"] {
            std::fs::create_dir(root.path().join(directory)).unwrap();
            std::fs::set_permissions(
                root.path().join(directory),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
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
            .env(CHILD, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        // Pin only this owned alias's kernel-identified peer/leader. The child
        // cannot start channel/model cases until this guard is armed.
        let native = tokio::time::timeout(Duration::from_secs(35), async {
            loop {
                let alias = std::fs::read_dir(root.path().join("runtime"))
                    .unwrap()
                    .filter_map(Result::ok)
                    .map(|e| e.path().join("app.sock"))
                    .find(|p| p.exists());
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
                if child.try_wait().unwrap().is_some() {
                    panic!("isolated startup failed before peer acquisition");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owned native peer deadline");
        std::fs::write(root.path().join("guarded"), b"ready").unwrap();
        let exit = tokio::time::timeout(Duration::from_secs(150), child.wait()).await;
        let raw_stage = std::fs::read_to_string(root.path().join("stage")).unwrap_or_default();
        if let Ok(line) = std::fs::read_to_string(root.path().join("fixture-failure-line"))
            && let Ok(line) = line.parse::<u32>()
            && line <= 10000
        {
            println!("TELEGRAM_TUI_INFLIGHT_FAILED fixture_line={line}");
        }
        if let Ok(bytes) = std::fs::read(root.path().join("tui-condition"))
            && bytes.len() <= 1024
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
            && let Some(condition) = value["condition"]
                .as_str()
                .filter(|name| matches!(*name, "local_menu" | "remote_menu" | "remote_retired"))
        {
            for key in [
                "header",
                "description",
                "destination",
                "allow",
                "menu",
                "working",
                "interrupt",
                "composer",
                "postAnswer",
                "stable",
            ] {
                if let Some(flag) = value[key].as_bool() {
                    println!("TELEGRAM_TUI_CONDITION condition={condition} {key}={flag}");
                }
            }
        }
        if let Ok(bytes) = std::fs::read(root.path().join("inflight-probe"))
            && bytes.len() <= 2048
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
            && let Some(phase) = value["phase"].as_str().filter(|phase| {
                matches!(*phase, "blocked" | "local_completed" | "released" | "final")
            })
        {
            for key in [
                "admitted",
                "blockedWrites",
                "bytesWritten",
                "flushed",
                "writtenUnconfirmed",
                "notSent",
                "unknown",
                "tuiAccept",
                "tuiOther",
                "resolved",
            ] {
                if let Some(count) = value[key].as_u64().filter(|count| *count <= 1_000_000) {
                    println!("TELEGRAM_TUI_INFLIGHT phase={phase} {key}={count}");
                }
            }
            for key in ["released", "closed"] {
                if let Some(flag) = value[key].as_bool() {
                    println!("TELEGRAM_TUI_INFLIGHT phase={phase} {key}={flag}");
                }
            }
        }
        let safe_stage = TUI_INFLIGHT_STAGES
            .iter()
            .copied()
            .find(|allowed| *allowed == raw_stage)
            .unwrap_or("unknown");
        assert!(
            matches!(exit, Ok(Ok(status)) if status.success()),
            "isolated in-flight TUI/channel path failed at stage {safe_stage}"
        );
        assert!(root.path().join("passed").exists());
        assert!(
            !root.path().join("child-panic").exists()
                && !root.path().join("fixture-failure-line").exists(),
            "isolated child or handler panic invalidates acceptance"
        );
        assert!(
            native.peer.gone() && native.wrapper.gone(),
            "primary lifecycle must finish before host exit"
        );
        println!(
            "TELEGRAM_TUI_INFLIGHT_PRODUCTION original_cli_01561=true control_transport_backpressure=true actual_tui_accept=true admitted_late_remote_decline=true no_result_reversal=true same_thread=true native_both_pending_not_proven=true"
        );
        return;
    }
    let user = PathBuf::from(std::env::var_os("HOME").unwrap());
    let home = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
    let root = user.parent().unwrap();
    let failure_line = root.join("fixture-failure-line");
    let panic_marker = root.join("child-panic");
    let child_panicked = Arc::new(AtomicBool::new(false));
    let hook_panicked = child_panicked.clone();
    std::panic::set_hook(Box::new(move |info| {
        // Tokio may isolate a transport handler panic from the test future.
        // Every child panic, including shared helpers, invalidates acceptance;
        // persist only a fixed marker, never its payload or source path.
        hook_panicked.store(true, Ordering::SeqCst);
        let _ = std::fs::write(&panic_marker, b"panic-observed");
        if let Some(location) = info.location()
            && location
                .file()
                .ends_with("telegram_tui_inflight_integration_tests.rs")
        {
            // The model transport may retry after a handler panic. Preserve the
            // first fixture failure rather than replacing it with the retry cap.
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&failure_line)
            {
                use std::io::Write;
                let _ = write!(file, "{}", location.line());
            }
        }
    }));
    tui_inflight_stage(root, "starting");
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
    let wire = Arc::new(FileScopeWire {
        transport: Arc::new(Wire::default()),
        journal: paths.root.join("channel/receipts.jsonl"),
        acknowledgments: AtomicUsize::new(0),
        cwd: cwd.clone(),
    });
    file_scope_seed(&cwd);
    let transport = &wire.transport;
    let (bot_addr, _bot) = server(
        Router::new()
            .route("/{*path}", post(file_scope_bot))
            .with_state(wire.clone()),
    )
    .await;
    let (model_addr, _model) = server(
        Router::new()
            .route("/v1/responses", post(tui_inflight_model))
            .with_state(wire.clone()),
    )
    .await;
    std::fs::create_dir(home.join("app-server-daemon")).unwrap();
    std::fs::write(
        home.join("app-server-daemon/settings.json"),
        r#"{"remoteControlEnabled":false,"updater":{"autoUpdateEnabled":false}}"#,
    )
    .unwrap();
    std::fs::write(home.join("config.toml"), format!(
        "model=\"gpt-6-astra\"\nmodel_provider=\"custom\"\napproval_policy=\"on-request\"\napprovals_reviewer=\"user\"\nsandbox_mode=\"read-only\"\ncheck_for_update_on_startup=false\n[features]\ndaemon_auto_start=false\n[model_providers.custom]\nname=\"Synthetic interaction integration\"\nbase_url=\"http://{model_addr}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nexperimental_bearer_token=\"synthetic-only\"\n[projects.{}]\ntrust_level=\"trusted\"\n", toml::Value::String(cwd.display().to_string()))).unwrap();
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
        panic!("isolated owner required")
    };
    let app = Application::start(prepared, home, paths, lock, None).unwrap();
    app.web.handle().telegram_api_for_test(bot_addr);
    app._runs
        .handle
        .launch(Connect {
            project: Some(cwd.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    let cookie = pair(&app).await;
    tui_inflight_stage(root, "source");
    let source = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let value = sources(&app, &cookie).await;
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
    .expect("original TUI subscribed source");
    let thread = source["source"]["targets"][0]["threadId"].clone();
    let run_id = source["runId"].clone();
    until(|| root.join("guarded").exists()).await;
    tui_inflight_stage(root, "configure");
    owner(
        &app,
        &cookie,
        "configure",
        json!({"token":"101:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"}),
    )
    .await;
    tui_inflight_stage(root, "pair");
    let pairing = owner(&app, &cookie, "pair", json!({})).await;
    fresh_poll(transport).await;
    fresh_poll(transport).await;
    transport.updates.lock().unwrap().push_back(update(
        1,
        1,
        202,
        pairing["pairing"]["command"].as_str().unwrap(),
    ));
    let candidate = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let s = status(&app, &cookie).await;
            if s["channel"]["binding"]["candidate"].is_object() {
                break s["channel"]["binding"]["candidate"].clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    tui_inflight_stage(root, "confirm");
    let current = sources(&app, &cookie).await;
    let confirmed = owner(&app,&cookie,"confirm",json!({"candidate":candidate,"runId":run_id,"threadId":thread,"sourceRevision":current["runs"][0]["source"]["revision"],"interactiveConsent":true})).await;
    assert_eq!(
        confirmed["channel"]["binding"]["active"]["interactiveConsent"],
        true
    );
    assert_eq!(
        confirmed["channel"]["binding"]["active"]["target"]["threadId"],
        thread
    );
    let qualified = confirmed["channel"]["binding"]["active"]["target"].clone();
    assert_eq!(qualified["runEpoch"], run_id);
    for field in ["transportEpoch", "subscriptionGeneration"] {
        assert!(
            qualified[field]
                .as_str()
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
        );
    }
    let (tx, rx) = oneshot::channel();
    app._runs
        .handle
        .sender
        .send(Request::Inspect(Box::new(move |runner| {
            assert!(tx.send(runner.active[0].0.terminal()).is_ok());
        })))
        .unwrap();
    let terminal = rx.await.unwrap();
    let mut page = terminal.attach().await.unwrap();
    let mut terminal_events = TuiOrderEvents::start(&mut page);
    let grant = terminal.claim(page.connection_id).await.unwrap();
    terminal
        .resize(page.connection_id, grant.generation, 50, 180)
        .await
        .unwrap();
    let mut input_sequence = 0;
    tui_order_bootstrap(
        &terminal,
        &page,
        &mut terminal_events,
        grant.generation,
        &mut input_sequence,
    )
    .await;
    let onboarding_sequence = input_sequence;
    let probe = app
        .web
        .handle()
        .answer_write_probe_for_test(uuid::Uuid::parse_str(run_id.as_str().unwrap()).unwrap())
        .expect("qualified test control transport probe required");
    let diagnose = |phase: &'static str| {
        assert!(matches!(
            phase,
            "blocked" | "local_completed" | "released" | "final"
        ));
        let state = probe.snapshot();
        std::fs::write(root.join("inflight-probe"), json!({"phase":phase,
            "admitted":state.admitted,"blockedWrites":state.blocked_writes,"bytesWritten":state.bytes_written,
            "flushed":state.flushed,"writtenUnconfirmed":state.written_unconfirmed,"notSent":state.not_sent,
            "unknown":state.unknown,"tuiAccept":state.tui_accept,"tuiOther":state.tui_other,
            "resolved":state.resolved,"released":state.released,"closed":state.closed}).to_string()).unwrap();
    };
    transport.permit_final.store(false, Ordering::SeqCst);
    fresh_poll(transport).await;
    transport
        .updates
        .lock()
        .unwrap()
        .push_back(update(2, 2, 202, "SYNTHETIC_INFLIGHT_INPUT"));
    until(|| transport.sent.lock().unwrap().len() == 1).await;
    let prompt = transport.sent.lock().unwrap()[0].clone();
    file_scope_prompt(prompt["text"].as_str().unwrap(), &cwd, "allowed");
    assert!(prompt.get("parse_mode").is_none());
    let buttons: Vec<_> = prompt["reply_markup"]["inline_keyboard"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r.as_array().unwrap())
        .collect();
    assert_eq!(buttons.len(), 2);
    assert!(buttons.iter().any(|b| b["text"] == "Allow once"));
    let reference = buttons.iter().find(|b| b["text"] == "Deny").unwrap()["callback_data"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        reference.len() == 32
            && reference
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    let task = status(&app, &cookie).await["channel"]["lastTask"]["id"].clone();
    tui_order_wait_menu(
        &terminal,
        &mut terminal_events,
        root,
        &cwd,
        "allowed",
        "local_menu",
    )
    .await;
    file_scope_files(&cwd, "allowed", false);
    file_scope_files(&cwd, "denied", false);
    fresh_poll(transport).await;
    fresh_poll(transport).await;
    let gate = probe.arm().expect("one private answer write gate required");
    tui_inflight_stage(root, "callback_blocked");
    transport
        .updates
        .lock()
        .unwrap()
        .push_back(interaction_callback(
            3,
            202,
            1,
            "callback_file_inflight_deny",
            &reference,
        ));
    let blocked = tokio::time::timeout(
        Duration::from_secs(10),
        probe.wait_for(|s| s.admitted == 1 && s.blocked_writes > 0),
    )
    .await;
    diagnose("blocked");
    let blocked = blocked.expect("admitted channel response must reach byte backpressure");
    assert_eq!(blocked.bytes_written, 0);
    assert_eq!(blocked.flushed, 0);
    assert_eq!(blocked.written_unconfirmed, 0);
    assert_eq!(blocked.tui_accept, 0);
    assert_eq!(blocked.tui_other, 0);
    assert_eq!(blocked.resolved, 0);
    assert_eq!(blocked.not_sent, 0);
    assert_eq!(blocked.unknown, 0);
    assert!(!blocked.released && !blocked.closed);
    let records = file_scope_receipts(&wire);
    let callback = records
        .iter()
        .find(|r| r["kind"] == "callback_received" && r["update_id"] == 3)
        .unwrap();
    assert_eq!(callback["task"], task);
    assert_eq!(callback["bot_id"], 101);
    let received = records
        .iter()
        .find(|r| r["kind"] == "received" && r["task"] == task)
        .unwrap();
    assert_eq!(
        received["key"],
        json!({"bot_id":101,"chat_id":202,"message_id":2})
    );
    assert_eq!(wire.acknowledgments.load(Ordering::SeqCst), 0);
    assert_eq!(transport.model.lock().unwrap().len(), 1);
    // With the remote response already admitted, verify this exact current
    // native menu again. Only then press its displayed y shortcut once.
    tui_order_wait_menu(
        &terminal,
        &mut terminal_events,
        root,
        &cwd,
        "allowed",
        "local_menu",
    )
    .await;
    tui_inflight_stage(root, "tui_answer");
    input_sequence += 1;
    terminal
        .input(
            page.connection_id,
            grant.generation,
            input_sequence,
            b"y".to_vec(),
        )
        .await
        .unwrap();
    let local = tokio::time::timeout(
        Duration::from_secs(10),
        probe.wait_for(|s| s.tui_accept == 1 && s.resolved == 1),
    )
    .await;
    diagnose("local_completed");
    local.expect("real TUI must emit the matched raw Accept and native resolution");
    until(|| transport.model.lock().unwrap().len() == 2).await;
    file_scope_files(&cwd, "allowed", true);
    file_scope_files(&cwd, "denied", false);
    let still_blocked = probe.snapshot();
    assert_eq!(still_blocked.admitted, 1);
    assert_eq!(still_blocked.bytes_written, 0);
    assert_eq!(still_blocked.flushed, 0);
    assert_eq!(still_blocked.written_unconfirmed, 0);
    assert_eq!(still_blocked.tui_accept, 1);
    assert_eq!(still_blocked.tui_other, 0);
    assert_eq!(still_blocked.not_sent, 0);
    assert_eq!(still_blocked.unknown, 0);
    assert!(!still_blocked.released && !still_blocked.closed);
    assert!(!transport.permit_final.load(Ordering::SeqCst));
    assert_eq!(wire.acknowledgments.load(Ordering::SeqCst), 0);
    assert_eq!(transport.sent.lock().unwrap().len(), 1);
    let current_task = status(&app, &cookie).await["channel"]["lastTask"].clone();
    assert_eq!(current_task["id"], task);
    assert_eq!(current_task["nativeCompleted"], false);
    let records = file_scope_receipts(&wire);
    assert!(
        records
            .iter()
            .any(|r| r["kind"] == "submitting" && r["task"] == task)
    );
    assert!(
        !records
            .iter()
            .any(|r| r["kind"] == "closed" && r["task"] == task)
    );

    tui_inflight_stage(root, "release");
    gate.release();
    let released = tokio::time::timeout(
        Duration::from_secs(10),
        probe.wait_for(|s| s.bytes_written > 0 && s.flushed == 1 && s.written_unconfirmed == 1),
    )
    .await;
    diagnose("released");
    let released = released.expect("already-admitted remote Decline must flush once");
    assert!(released.released && !released.closed);
    assert_eq!(released.admitted, 1);
    assert_eq!(released.not_sent, 0);
    assert_eq!(released.unknown, 0);
    assert_eq!(released.tui_accept, 1);
    assert_eq!(released.tui_other, 0);
    assert_eq!(released.resolved, 1);
    until(|| wire.acknowledgments.load(Ordering::SeqCst) == 1).await;
    assert_eq!(transport.model.lock().unwrap().len(), 2);
    file_scope_files(&cwd, "allowed", true);
    file_scope_files(&cwd, "denied", false);

    tui_inflight_stage(root, "duplicate");
    transport
        .updates
        .lock()
        .unwrap()
        .push_back(interaction_callback(
            4,
            202,
            1,
            "callback_file_inflight_old_ref",
            &reference,
        ));
    until(|| {
        file_scope_receipts(&wire)
            .iter()
            .any(|r| r["kind"] == "rejected" && r["update_id"] == 4)
    })
    .await;
    fresh_poll(transport).await;
    assert_eq!(wire.acknowledgments.load(Ordering::SeqCst), 1);
    assert_eq!(transport.model.lock().unwrap().len(), 2);
    assert_eq!(probe.snapshot().admitted, 1);
    assert!(!transport.permit_final.load(Ordering::SeqCst));
    assert_eq!(transport.sent.lock().unwrap().len(), 1);
    tui_inflight_stage(root, "final");
    transport.permit_final.store(true, Ordering::SeqCst);
    until(|| transport.sent.lock().unwrap().len() == 2).await;
    assert_eq!(
        transport.sent.lock().unwrap()[1]["text"],
        "SYNTHETIC_INFLIGHT_FINAL"
    );
    assert!(
        transport.sent.lock().unwrap()[1]
            .get("reply_markup")
            .is_none()
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if status(&app, &cookie).await["channel"]["lastTask"]["nativeCompleted"] == true {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    until(|| {
        file_scope_receipts(&wire)
            .iter()
            .any(|r| r["kind"] == "closed" && r["task"] == task)
    })
    .await;
    let records = file_scope_receipts(&wire);
    for kind in ["received", "submitting", "callback_received", "closed"] {
        assert_eq!(records.iter().filter(|r| r["kind"] == kind).count(), 1);
    }
    assert_eq!(
        records
            .iter()
            .filter(|r| r["kind"] == "rejected" && r["update_id"] == 4)
            .count(),
        1
    );
    assert_eq!(input_sequence, onboarding_sequence + 1);
    assert_eq!(wire.acknowledgments.load(Ordering::SeqCst), 1);
    assert_eq!(transport.model.lock().unwrap().len(), 2);
    let final_probe = probe.snapshot();
    diagnose("final");
    assert_eq!(final_probe.admitted, 1);
    assert_eq!(final_probe.flushed, 1);
    assert_eq!(final_probe.written_unconfirmed, 1);
    assert_eq!(final_probe.not_sent, 0);
    assert_eq!(final_probe.unknown, 0);
    assert_eq!(final_probe.tui_accept, 1);
    assert_eq!(final_probe.tui_other, 0);
    assert_eq!(final_probe.resolved, 1);
    assert!(final_probe.released && !final_probe.closed);
    assert_eq!(
        status(&app, &cookie).await["channel"]["binding"]["active"]["target"],
        qualified
    );
    let current = sources(&app, &cookie).await;
    assert_eq!(current["runs"][0]["runId"], run_id);
    assert_eq!(
        current["runs"][0]["source"]["targets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        current["runs"][0]["source"]["targets"][0]["threadId"],
        thread
    );
    let raw = std::fs::read_to_string(&wire.journal).unwrap();
    assert!(!raw.contains(thread.as_str().unwrap()) && !raw.contains(&reference));
    file_scope_files(&cwd, "allowed", true);
    file_scope_files(&cwd, "denied", false);
    tui_inflight_stage(root, "shutdown");
    drop(terminal_events);
    drop(app);
    assert!(
        !child_panicked.load(Ordering::SeqCst)
            && !root.join("child-panic").exists()
            && !root.join("fixture-failure-line").exists(),
        "child or handler panic invalidates acceptance"
    );
    tui_inflight_stage(root, "passed");
    std::fs::write(root.join("passed"), b"passed").unwrap();
}
