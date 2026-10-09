// Included after the file-scope and TUI-order fixtures. This is same-process,
// same-loaded-Thread recovery, not a transport restart or a Cleanup/confirm race.
const REBIND_STAGES: &[&str] = &[
    "starting",
    "source",
    "configure",
    "pair",
    "confirm",
    "old_menu",
    "revoke",
    "rebind",
    "stale_inputs",
    "new_waiting",
    "local_allow",
    "old_final",
    "new_final",
    "shutdown",
    "passed",
];
fn rebind_stage(root: &Path, name: &'static str) {
    assert!(REBIND_STAGES.contains(&name));
    std::fs::write(root.join("stage"), name).unwrap();
}
#[derive(Default)]
struct RebindFinalGate {
    open: AtomicBool,
    changed: tokio::sync::Notify,
}
impl RebindFinalGate {
    async fn wait(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
    fn release(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
    }
}
struct RebindWire {
    scope: Arc<FileScopeWire>,
    max_offset: AtomicUsize,
    ordinary_attempts: AtomicUsize,
    old_final: RebindFinalGate,
    new_final: RebindFinalGate,
}
fn rebind_notice(text: &str) {
    let command = text
        .strip_prefix("Task is waiting. Cancel before submission with:\n/cancel ")
        .and_then(|rest| {
            rest.strip_suffix("\nAfter submission starts, cancellation is unavailable.")
        })
        .expect("exact static queue notice required");
    assert!(
        command.len() == 32
            && command
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
}
async fn rebind_bot(
    State(wire): State<Arc<RebindWire>>,
    uri: Uri,
    body: Bytes,
) -> axum::Json<Value> {
    let request: Value = serde_json::from_slice(&body).unwrap();
    if uri.path().ends_with("/getUpdates") {
        if let Some(offset) = request["offset"].as_u64() {
            assert!(offset <= 7, "unexpected update consumption");
            // Production advances offset only after durable handling/duplicate
            // lookup. This observes consumption without granting any authority.
            wire.max_offset.fetch_max(offset as usize, Ordering::SeqCst);
        }
    } else if uri.path().ends_with("/sendMessage") {
        let text = request["text"].as_str().unwrap();
        let sent = wire.scope.transport.sent.lock().unwrap().len();
        match sent {
            0 => file_scope_prompt(text, &wire.scope.cwd, "allowed"),
            1 => {
                rebind_notice(text);
                assert!(request.get("reply_markup").is_none());
            }
            2 => {
                assert_eq!(text, "SYNTHETIC_REBIND_NEW_FINAL");
                assert!(wire.old_final.open.load(Ordering::SeqCst));
                assert!(wire.new_final.open.load(Ordering::SeqCst));
                assert_eq!(wire.ordinary_attempts.load(Ordering::SeqCst), 3);
                assert!(request.get("reply_markup").is_none());
            }
            _ => panic!("unexpected extra Telegram delivery"),
        }
    }
    file_scope_bot(State(wire.scope.clone()), uri, body).await
}
async fn rebind_model(State(wire): State<Arc<RebindWire>>, body: Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap();
    let transport = &wire.scope.transport;
    let sequence = transport.model_sequence.fetch_add(1, Ordering::SeqCst) + 1;
    assert!(sequence <= 8);
    let title = request["text"]["format"]["type"] == "json_schema"
        && request["text"]["format"]["schema"]["properties"]["title"]["type"] == "string"
        && request["text"]["format"]["schema"]["required"] == json!(["title"]);
    let turn = if title {
        transport.title_requests.fetch_add(1, Ordering::SeqCst);
        0
    } else {
        // Count attempts before all assertions: a native retry after a handler
        // panic must not disappear behind the successful-request collection.
        let attempt = wire.ordinary_attempts.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(attempt <= 3, "native continuation/replay bound exceeded");
        let input = request["input"].as_array().unwrap();
        let user_count = |marker: &str| {
            input
                .iter()
                .filter(|item| {
                    item["role"] == "user" && item["content"].to_string().contains(marker)
                })
                .count()
        };
        assert_eq!(user_count("SYNTHETIC_REBIND_OLD_INPUT"), 1);
        assert_eq!(
            user_count("SYNTHETIC_REBIND_NEW_INPUT"),
            usize::from(attempt == 3)
        );
        let output = input.iter().find(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "call_file_scope_1"
        });
        if attempt == 1 {
            assert!(output.is_none());
            file_scope_files(&wire.scope.cwd, "allowed", false);
        } else {
            let text = output.expect("actual native patch result required")["output"].to_string();
            assert!(
                text.contains("Success")
                    && !text.contains("rejected")
                    && !text.contains("declined")
            );
            file_scope_files(&wire.scope.cwd, "allowed", true);
        }
        file_scope_files(&wire.scope.cwd, "denied", false);
        if attempt == 3 {
            assert!(wire.old_final.open.load(Ordering::SeqCst));
            assert!(input.iter().any(|item| {
                item["role"] == "assistant"
                    && item["content"]
                        .to_string()
                        .contains("SYNTHETIC_REBIND_OLD_FINAL")
            }));
        }
        let records = file_scope_receipts(&wire.scope);
        assert_eq!(
            records
                .iter()
                .filter(|r| r["kind"] == "callback_received")
                .count(),
            0
        );
        assert_eq!(
            records.iter().filter(|r| r["kind"] == "submitting").count(),
            if attempt == 3 { 2 } else { 1 }
        );
        if attempt >= 2 {
            assert_eq!(records.iter().filter(|r| r["kind"] == "closed").count(), 1);
        }
        transport.model.lock().unwrap().push(request);
        attempt
    };
    let stream = async_stream::stream! {
        let event = |value: Value| Bytes::from(format!("data: {value}\n\n"));
        let id = format!("file_scope_response_{sequence}");
        yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":id,"model":"gpt-6-astra"}})));
        match turn {
            2 => wire.old_final.wait().await,
            3 => wire.new_final.wait().await,
            _ => {}
        }
        let item = if turn == 1 {
            let args = json!({"cmd":file_scope_patch("allowed"),"login":false,"yield_time_ms":1000});
            json!({"id":format!("item_file_{sequence}"),"type":"function_call","call_id":"call_file_scope_1","name":"exec_command","arguments":args.to_string()})
        } else {
            let text = match turn {
                0 => r#"{"title":"Synthetic rebind recovery"}"#,
                2 => "SYNTHETIC_REBIND_OLD_FINAL",
                3 => "SYNTHETIC_REBIND_NEW_FINAL",
                _ => unreachable!(),
            };
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
#[ignore = "explicit original CLI 0.156.1; same loaded Thread rebind with unresolved native work"]
async fn installed_cli_production_telegram_rebind_recovery_path() {
    const CHILD: &str = "CODEX_TELEGRAM_REBIND_CHILD";
    const TEST: &str = "workbench::application::tests::telegram_native_integration::installed_cli_production_telegram_rebind_recovery_path";
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
            println!("TELEGRAM_REBIND_FAILED fixture_line={line}");
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
        let safe_stage = REBIND_STAGES
            .iter()
            .copied()
            .find(|allowed| *allowed == raw_stage)
            .unwrap_or("unknown");
        assert!(
            matches!(exit, Ok(Ok(status)) if status.success()),
            "isolated same-Thread rebind path failed at stage {safe_stage}"
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
            "TELEGRAM_REBIND_PRODUCTION original_cli_01561=true same_loaded_thread=true rebind_generation_changed=true old_native_continues=true stale_inputs_excluded=true new_queued_dialog_completed=true old_final_withheld=true"
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
                .ends_with("telegram_rebind_integration_tests.rs")
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
    rebind_stage(root, "starting");
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
    file_scope_seed(&cwd);
    let wire = Arc::new(RebindWire {
        scope,
        max_offset: AtomicUsize::new(0),
        ordinary_attempts: AtomicUsize::new(0),
        old_final: RebindFinalGate::default(),
        new_final: RebindFinalGate::default(),
    });
    let transport = &wire.scope.transport;
    let (bot_addr, _bot) = server(
        Router::new()
            .route("/{*path}", post(rebind_bot))
            .with_state(wire.clone()),
    )
    .await;
    let (model_addr, _model) = server(
        Router::new()
            .route("/v1/responses", post(rebind_model))
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
    rebind_stage(root, "source");
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
    rebind_stage(root, "configure");
    owner(
        &app,
        &cookie,
        "configure",
        json!({"token":"101:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"}),
    )
    .await;
    rebind_stage(root, "pair");
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
    rebind_stage(root, "confirm");
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
    let old_generation = confirmed["channel"]["binding"]["generation"].clone();
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
    rebind_stage(root, "old_menu");
    fresh_poll(transport).await;
    transport
        .updates
        .lock()
        .unwrap()
        .push_back(update(2, 2, 202, "SYNTHETIC_REBIND_OLD_INPUT"));
    until(|| transport.sent.lock().unwrap().len() == 1).await;
    let prompt = transport.sent.lock().unwrap()[0].clone();
    file_scope_prompt(prompt["text"].as_str().unwrap(), &cwd, "allowed");
    let buttons: Vec<_> = prompt["reply_markup"]["inline_keyboard"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|row| row.as_array().unwrap())
        .collect();
    assert_eq!(buttons.len(), 2);
    assert!(buttons.iter().any(|button| button["text"] == "Allow once"));
    let reference = buttons
        .iter()
        .find(|button| button["text"] == "Deny")
        .unwrap()["callback_data"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        reference.len() == 32
            && reference
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
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
    let records = file_scope_receipts(&wire.scope);
    let old_task = records
        .iter()
        .find(|r| r["kind"] == "received" && r["update_id"] == 2)
        .unwrap()["task"]
        .clone();
    assert!(
        records
            .iter()
            .any(|r| r["kind"] == "submitting" && r["task"] == old_task)
    );
    assert!(!records.iter().any(|r| r["kind"] == "closed"));
    // Observe successful synthetic HTTP delivery and polling progress. This is
    // not a private Service reference-activation barrier and grants no authority.
    fresh_poll(transport).await;
    fresh_poll(transport).await;

    rebind_stage(root, "revoke");
    let revoked = owner(&app, &cookie, "revoke", json!({})).await;
    assert!(revoked["channel"]["binding"]["active"].is_null());
    until(|| {
        file_scope_receipts(&wire.scope)
            .iter()
            .any(|r| r["kind"] == "closed" && r["task"] == old_task)
    })
    .await;
    let old_view = status(&app, &cookie).await["channel"]["lastTask"].clone();
    assert_eq!(old_view["id"], old_task);
    assert_eq!(old_view["phase"], "abandoned");
    assert_eq!(old_view["delivery"], "withheld");
    assert_eq!(old_view["nativeCompleted"], false);
    assert_eq!(old_view["reason"], "channel_binding_changed");
    tui_order_wait_menu(
        &terminal,
        &mut terminal_events,
        root,
        &cwd,
        "allowed",
        "local_menu",
    )
    .await;
    let current = sources(&app, &cookie).await;
    assert_eq!(
        current["runs"][0]["source"]["targets"][0]["threadId"],
        thread
    );
    assert_eq!(current["runs"][0]["source"]["targets"][0]["busy"], true);
    assert_eq!(wire.ordinary_attempts.load(Ordering::SeqCst), 1);
    file_scope_files(&cwd, "allowed", false);

    // The old channel actor has closed before rebind. Its still-unresolved
    // native approval crosses the new generation; no Cleanup race is claimed.
    rebind_stage(root, "rebind");
    let pairing = owner(&app, &cookie, "pair", json!({})).await;
    fresh_poll(transport).await;
    fresh_poll(transport).await;
    transport.updates.lock().unwrap().push_back(update(
        3,
        3,
        202,
        pairing["pairing"]["command"].as_str().unwrap(),
    ));
    let candidate = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let s = status(&app, &cookie).await;
            if s["channel"]["binding"]["candidate"].is_object() {
                break s["channel"]["binding"]["candidate"].clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let current = sources(&app, &cookie).await;
    let confirmed = owner(&app, &cookie, "confirm", json!({"candidate":candidate,"runId":run_id,"threadId":thread,"sourceRevision":current["runs"][0]["source"]["revision"],"interactiveConsent":true})).await;
    let replacement = confirmed["channel"]["binding"].clone();
    assert_ne!(replacement["generation"], old_generation);
    assert_eq!(replacement["active"]["target"], qualified);
    assert_eq!(replacement["active"]["interactiveConsent"], true);
    assert_eq!(replacement["active"]["userId"], 202);
    assert_eq!(replacement["active"]["chatId"], 202);
    assert_eq!(replacement["active"]["botId"], 101);
    fresh_poll(transport).await;
    fresh_poll(transport).await;

    rebind_stage(root, "stale_inputs");
    transport
        .updates
        .lock()
        .unwrap()
        .push_back(interaction_callback(
            4,
            202,
            1,
            "callback_file_rebind_old",
            &reference,
        ));
    until(|| {
        file_scope_receipts(&wire.scope)
            .iter()
            .any(|r| r["kind"] == "rejected" && r["update_id"] == 4)
    })
    .await;
    transport
        .updates
        .lock()
        .unwrap()
        .push_back(update(5, 2, 202, "SYNTHETIC_REBIND_OLD_INPUT"));
    until(|| wire.max_offset.load(Ordering::SeqCst) >= 6).await;
    let records = file_scope_receipts(&wire.scope);
    assert_eq!(
        records.iter().filter(|r| r["kind"] == "received").count(),
        1
    );
    assert_eq!(
        records.iter().filter(|r| r["kind"] == "submitting").count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|r| r["kind"] == "callback_received")
            .count(),
        0
    );
    assert!(!records.iter().any(|r| r["update_id"] == 5));
    assert_eq!(wire.scope.acknowledgments.load(Ordering::SeqCst), 0);
    assert_eq!(transport.sent.lock().unwrap().len(), 1);
    assert_eq!(wire.ordinary_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(
        status(&app, &cookie).await["channel"]["binding"],
        replacement
    );
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

    rebind_stage(root, "new_waiting");
    transport
        .updates
        .lock()
        .unwrap()
        .push_back(update(6, 6, 202, "SYNTHETIC_REBIND_NEW_INPUT"));
    until(|| transport.sent.lock().unwrap().len() == 2).await;
    let notice = transport.sent.lock().unwrap()[1].clone();
    rebind_notice(notice["text"].as_str().unwrap());
    until(|| wire.max_offset.load(Ordering::SeqCst) >= 7).await;
    let records = file_scope_receipts(&wire.scope);
    let new_task = records
        .iter()
        .find(|r| r["kind"] == "received" && r["update_id"] == 6)
        .unwrap()["task"]
        .clone();
    assert_ne!(new_task, old_task);
    assert!(
        !records
            .iter()
            .any(|r| r["kind"] == "submitting" && r["task"] == new_task)
    );
    assert!(
        !records
            .iter()
            .any(|r| r["kind"] == "closed" && r["task"] == new_task)
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = status(&app, &cookie).await;
            if s["channel"]["lastTask"]["id"] == new_task
                && s["channel"]["lastTask"]["phase"] == "waiting"
            {
                assert_eq!(s["channel"]["binding"], replacement);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(wire.ordinary_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(
        sources(&app, &cookie).await["runs"][0]["source"]["targets"][0]["busy"],
        true
    );

    rebind_stage(root, "local_allow");
    tui_order_wait_menu(
        &terminal,
        &mut terminal_events,
        root,
        &cwd,
        "allowed",
        "local_menu",
    )
    .await;
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
    until(|| transport.model.lock().unwrap().len() == 2).await;
    assert_eq!(wire.ordinary_attempts.load(Ordering::SeqCst), 2);
    file_scope_files(&cwd, "allowed", true);
    file_scope_files(&cwd, "denied", false);
    assert!(!wire.old_final.open.load(Ordering::SeqCst));
    assert!(!wire.new_final.open.load(Ordering::SeqCst));
    assert_eq!(transport.sent.lock().unwrap().len(), 2);
    assert_eq!(wire.scope.acknowledgments.load(Ordering::SeqCst), 0);
    let records = file_scope_receipts(&wire.scope);
    assert!(
        !records
            .iter()
            .any(|r| r["kind"] == "submitting" && r["task"] == new_task)
    );
    assert_eq!(
        status(&app, &cookie).await["channel"]["binding"],
        replacement
    );

    rebind_stage(root, "old_final");
    wire.old_final.release();
    // The queued task must advance automatically only after native becomes
    // ready. Its model context independently proves the old final was retained.
    until(|| transport.model.lock().unwrap().len() == 3).await;
    assert_eq!(wire.ordinary_attempts.load(Ordering::SeqCst), 3);
    assert_eq!(transport.sent.lock().unwrap().len(), 2);
    assert_eq!(
        status(&app, &cookie).await["channel"]["binding"],
        replacement
    );
    let records = file_scope_receipts(&wire.scope);
    assert!(
        records
            .iter()
            .any(|r| r["kind"] == "submitting" && r["task"] == new_task)
    );
    assert!(
        !records
            .iter()
            .any(|r| r["kind"] == "closed" && r["task"] == new_task)
    );
    let view = status(&app, &cookie).await["channel"]["lastTask"].clone();
    assert_eq!(view["id"], new_task);
    assert_eq!(view["nativeCompleted"], false);
    assert!(!wire.new_final.open.load(Ordering::SeqCst));

    rebind_stage(root, "new_final");
    wire.new_final.release();
    until(|| transport.sent.lock().unwrap().len() == 3).await;
    until(|| {
        file_scope_receipts(&wire.scope)
            .iter()
            .any(|r| r["kind"] == "closed" && r["task"] == new_task)
    })
    .await;
    let s = status(&app, &cookie).await;
    assert_eq!(s["channel"]["binding"], replacement);
    assert_eq!(s["channel"]["lastTask"]["id"], new_task);
    assert_eq!(s["channel"]["lastTask"]["nativeCompleted"], true);
    assert_eq!(s["channel"]["lastTask"]["phase"], "completed");
    assert_eq!(s["channel"]["lastTask"]["delivery"], "confirmed");
    assert_eq!(s["channel"]["queued"], 0);
    let records = file_scope_receipts(&wire.scope);
    for kind in ["received", "submitting", "closed"] {
        assert_eq!(records.iter().filter(|r| r["kind"] == kind).count(), 2);
    }
    assert_eq!(
        records
            .iter()
            .filter(|r| r["kind"] == "callback_received")
            .count(),
        0
    );
    let rejected: Vec<_> = records
        .iter()
        .filter(|r| r["kind"] == "rejected")
        .map(|r| r["update_id"].as_i64().unwrap())
        .collect();
    assert_eq!(rejected, vec![1, 3, 4]);
    assert_eq!(records.len(), 9, "only exact receipt kinds may be appended");
    for (task, update_id, message_id) in [(&old_task, 2, 2), (&new_task, 6, 6)] {
        let received = records
            .iter()
            .find(|r| r["kind"] == "received" && r["task"] == *task)
            .unwrap();
        assert_eq!(received["update_id"], update_id);
        assert_eq!(
            received["key"],
            json!({"bot_id":101,"chat_id":202,"message_id":message_id})
        );
        assert_eq!(
            records
                .iter()
                .filter(|r| r["kind"] == "submitting" && r["task"] == *task)
                .count(),
            1
        );
        assert_eq!(
            records
                .iter()
                .filter(|r| r["kind"] == "closed" && r["task"] == *task)
                .count(),
            1
        );
    }
    assert_eq!(wire.scope.acknowledgments.load(Ordering::SeqCst), 0);
    assert_eq!(input_sequence, onboarding_sequence + 1);
    assert_eq!(transport.model.lock().unwrap().len(), 3);
    assert_eq!(wire.ordinary_attempts.load(Ordering::SeqCst), 3);
    let sent = transport.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[2]["text"], "SYNTHETIC_REBIND_NEW_FINAL");
    assert!(
        sent.iter()
            .all(|message| message["text"] != "SYNTHETIC_REBIND_OLD_FINAL")
    );
    let current = sources(&app, &cookie).await;
    assert_eq!(current["runs"].as_array().unwrap().len(), 1);
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
    let raw = std::fs::read_to_string(&wire.scope.journal).unwrap();
    for private in [
        thread.as_str().unwrap(),
        run_id.as_str().unwrap(),
        qualified["transportEpoch"].as_str().unwrap(),
        qualified["subscriptionGeneration"].as_str().unwrap(),
        reference.as_str(),
    ] {
        assert!(
            !raw.contains(private),
            "native identity/reference entered receipts"
        );
    }
    file_scope_files(&cwd, "allowed", true);
    file_scope_files(&cwd, "denied", false);
    rebind_stage(root, "shutdown");
    drop(terminal_events);
    drop(app);
    assert!(
        !child_panicked.load(Ordering::SeqCst)
            && !root.join("child-panic").exists()
            && !root.join("fixture-failure-line").exists(),
        "child or handler panic invalidates acceptance"
    );
    rebind_stage(root, "passed");
    std::fs::write(root.join("passed"), b"passed").unwrap();
}
