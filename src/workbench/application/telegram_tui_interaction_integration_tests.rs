// Included in telegram_native_integration after the mixed file-scope fixture.
// Ordered original TUI/production-channel responses, not simultaneous races.
// Research checkout 2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4 maps the TUI
// file menu's No/Esc to Cancel, not Decline. This fixture never presses it.
fn tui_order_menu(text: &str, cwd: &Path, case: &str) -> bool {
    let Some((_, menu)) = text.rsplit_once("Would you like to make the following edits?") else {
        return false;
    };
    menu.contains("Description:")
        && menu.contains(&format!(
            "Destination: {}",
            cwd.join(case).join("fixture-add.txt").display()
        ))
        && menu
            .lines()
            .any(|line| line.trim_start_matches([' ', '›']).trim() == "1. Yes, proceed (y)")
        && menu.contains("2. Yes, and don't ask again for these files")
        && menu.contains("3. No, and tell Codex what to do differently")
        && menu.contains("Press enter to confirm")
}
async fn tui_order_screen(terminal: &crate::workbench::terminal::TerminalHandle) -> String {
    let view = terminal
        .attach()
        .await
        .expect("synthetic TUI snapshot unavailable");
    let mut parser = vt100::Parser::new(view.snapshot.rows, view.snapshot.cols, 0);
    parser.process(&view.snapshot.screen);
    parser.process(&view.snapshot.replay);
    // Only the current VT screen; never search append-only PTY output/history.
    parser.screen().contents()
}
const TUI_ORDER_STAGES: &[&str] = &[
    "starting",
    "source",
    "configure",
    "pair",
    "confirm",
    "first_turn",
    "second_turn",
    "local_menu",
    "remote_menu",
    "remote_retired",
    "shutdown",
    "passed",
];
fn tui_order_stage(root: &Path, name: &'static str) {
    assert!(TUI_ORDER_STAGES.contains(&name));
    std::fs::write(root.join("stage"), name).unwrap();
}
struct TuiOrderEvents {
    changed: tokio::sync::watch::Receiver<u64>,
    task: tokio::task::JoinHandle<()>,
}
impl TuiOrderEvents {
    fn start(page: &mut crate::workbench::terminal::Attachment) -> Self {
        let (unused, replacement) = tokio::sync::mpsc::channel(1);
        drop(unused);
        let mut events = std::mem::replace(&mut page.events, replacement);
        let (notify, changed) = tokio::sync::watch::channel(0u64);
        let task = tokio::spawn(async move {
            // Continuously drain the grant-owning subscription even while the
            // test waits on model, receipts, Bot polling, or status HTTP calls.
            while events.recv().await.is_some() {
                notify.send_modify(|sequence| *sequence = sequence.wrapping_add(1));
            }
        });
        Self { changed, task }
    }
    async fn next(&mut self) {
        self.changed
            .changed()
            .await
            .expect("synthetic TUI event stream ended");
    }
}
impl Drop for TuiOrderEvents {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn tui_order_composer(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim_start();
        line == "›" || (line.starts_with("› ") && !line.starts_with("› 1."))
    })
}
fn tui_order_post_answer(text: &str) -> bool {
    text.contains("Working") || text.contains("esc to interrupt") || tui_order_composer(text)
}
fn tui_order_diagnostic(
    root: &Path,
    name: &'static str,
    text: &str,
    cwd: &Path,
    case: &str,
    stable: bool,
) {
    assert!(matches!(
        name,
        "local_menu" | "remote_menu" | "remote_retired"
    ));
    let diagnostic = json!({"condition": name,
        "header": text.contains("Would you like to make the following edits?"),
        "description": text.contains("Description:"),
        "destination": text.contains(&format!("Destination: {}", cwd.join(case).join("fixture-add.txt").display())),
        "allow": text.contains("1. Yes, proceed (y)"),
        "menu": tui_order_menu(text, cwd, case),
        "working": text.contains("Working"),
        "interrupt": text.contains("esc to interrupt"),
        "composer": tui_order_composer(text),
        "postAnswer": tui_order_post_answer(text),
        "stable": stable});
    // Only fixed field names, a whitelisted enum, and booleans reach disk.
    std::fs::write(root.join("tui-condition"), diagnostic.to_string()).unwrap();
}
async fn tui_order_wait_menu(
    terminal: &crate::workbench::terminal::TerminalHandle,
    events: &mut TuiOrderEvents,
    root: &Path,
    cwd: &Path,
    case: &str,
    name: &'static str,
) {
    tui_order_stage(root, name);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let text = tui_order_screen(terminal).await;
            tui_order_diagnostic(root, name, &text, cwd, case, false);
            if tui_order_menu(&text, cwd, case) {
                break;
            }
            events.next().await;
        }
    })
    .await
    .expect("synthetic TUI menu visibility deadline; raw screen suppressed");
}
async fn tui_order_wait_retired(
    terminal: &crate::workbench::terminal::TerminalHandle,
    events: &mut TuiOrderEvents,
    root: &Path,
    cwd: &Path,
    case: &str,
) {
    tui_order_stage(root, "remote_retired");
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut since = None;
        loop {
            let text = tui_order_screen(terminal).await;
            let overlay = text.contains("Would you like to make the following edits?")
                || text.contains("1. Yes, proceed (y)");
            tui_order_diagnostic(root, "remote_retired", &text, cwd, case, false);
            assert!(
                !(since.is_some() && overlay),
                "retired synthetic approval menu reappeared"
            );
            if !overlay && tui_order_post_answer(&text) {
                let first = since.get_or_insert_with(tokio::time::Instant::now);
                let stable = first.elapsed() >= Duration::from_millis(300);
                tui_order_diagnostic(root, "remote_retired", &text, cwd, case, stable);
                if stable {
                    break;
                }
                tokio::select! {
                    () = events.next() => {},
                    () = tokio::time::sleep_until(*first + Duration::from_millis(300)) => {},
                }
            } else {
                // Blank repaint frames cannot establish retirement. Require a
                // real post-answer indicator throughout the stability interval.
                since = None;
                tui_order_diagnostic(root, "remote_retired", &text, cwd, case, false);
                events.next().await;
            }
        }
    })
    .await
    .expect("synthetic TUI menu retirement deadline; raw screen suppressed");
}
async fn tui_order_bootstrap(
    terminal: &crate::workbench::terminal::TerminalHandle,
    page: &crate::workbench::terminal::Attachment,
    events: &mut TuiOrderEvents,
    generation: u64,
    sequence: &mut u64,
) {
    let mut themed = false;
    let mut trusted = false;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let text = tui_order_screen(terminal).await;
            let theme = text.contains("Choose your style") || text.contains("Select a theme");
            let trust = text.contains("Do you trust")
                || text.contains("Do you want to work in this directory")
                || text.contains("Trust this folder?");
            if (!themed && theme) || (!trusted && trust) {
                themed |= theme;
                trusted |= trust;
                *sequence += 1;
                terminal
                    .input(page.connection_id, generation, *sequence, b"\r".to_vec())
                    .await
                    .expect("synthetic onboarding key rejected");
            } else if !theme && !trust && text.contains("OpenAI Codex") && text.contains('›') {
                break;
            }
            events.next().await;
        }
    })
    .await
    .expect("synthetic TUI onboarding deadline; raw screen suppressed");
}
async fn tui_order_model(State(wire): State<Arc<FileScopeWire>>, body: Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap();
    let transport = &wire.transport;
    let sequence = transport.model_sequence.fetch_add(1, Ordering::SeqCst) + 1;
    assert!(sequence <= 12);
    let title = request["text"]["format"]["type"] == "json_schema"
        && request["text"]["format"]["schema"]["properties"]["title"]["type"] == "string"
        && request["text"]["format"]["schema"]["required"] == json!(["title"]);
    let (turn, tool) = if title {
        transport.title_requests.fetch_add(1, Ordering::SeqCst);
        (0, false)
    } else {
        let input = request["input"].as_array().unwrap();
        let turn = input
            .iter()
            .rev()
            .filter(|item| item["role"] == "user")
            .find_map(|item| {
                let content = item["content"].to_string();
                [("SYNTHETIC_TUI_ALLOW", 1), ("SYNTHETIC_REMOTE_DENY", 2)]
                    .into_iter()
                    .find_map(|(marker, turn)| content.contains(marker).then_some(turn))
            })
            .expect("known ordered TUI/channel turn");
        let call = format!("call_file_scope_{turn}");
        let output = input
            .iter()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == call);
        if let Some(output) = output {
            assert_eq!(
                file_scope_receipts(&wire)
                    .iter()
                    .filter(|r| r["kind"] == "callback_received")
                    .count(),
                turn - 1
            );
            let text = output["output"].to_string();
            if turn == 2 {
                assert!(
                    text.contains("rejected") || text.contains("declined"),
                    "native patch must report denial"
                );
            } else {
                assert!(
                    text.contains("Success")
                        && !text.contains("rejected")
                        && !text.contains("declined"),
                    "native patch must report successful application"
                );
            }
            file_scope_files(
                &wire.cwd,
                if turn == 1 { "allowed" } else { "denied" },
                turn == 1,
            );
        }
        let tool = output.is_none();
        let mut requests = transport.model.lock().unwrap();
        assert!(requests.len() < 4);
        requests.push(request);
        (turn, tool)
    };
    let stream = async_stream::stream! {
        let event = |value: Value| Bytes::from(format!("data: {value}\n\n"));
        let id = format!("file_scope_response_{sequence}");
        yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":id,"model":"gpt-6-astra"}})));
        if !tool && !title {
            while !wire.transport.permit_final.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        let item = if tool {
            let args = json!({"cmd":file_scope_patch(if turn == 1 { "allowed" } else { "denied" }),"login":false,"yield_time_ms":1000});
            json!({"id":format!("item_file_{sequence}"),"type":"function_call","call_id":format!("call_file_scope_{turn}"),"name":"exec_command","arguments":args.to_string()})
        } else {
            let text = if title { r#"{"title":"Synthetic file scope"}"#.to_owned() } else { format!("SYNTHETIC_TUI_ORDER_FINAL_{turn}") };
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
#[ignore = "explicit isolated original CLI 0.156.1; ordered real TUI and production Telegram file responses"]
async fn installed_cli_production_telegram_tui_response_order_path() {
    const CHILD: &str = "CODEX_TELEGRAM_TUI_ORDER_CHILD";
    const TEST: &str = "workbench::application::tests::telegram_native_integration::installed_cli_production_telegram_tui_response_order_path";
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
            println!("TELEGRAM_TUI_ORDER_FAILED fixture_line={line}");
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
        let safe_stage = TUI_ORDER_STAGES
            .iter()
            .copied()
            .find(|allowed| *allowed == raw_stage)
            .unwrap_or("unknown");
        assert!(
            matches!(exit, Ok(Ok(status)) if status.success()),
            "isolated ordered TUI/channel path failed at stage {safe_stage}"
        );
        assert!(root.path().join("passed").exists());
        assert!(
            native.peer.gone() && native.wrapper.gone(),
            "primary lifecycle must finish before host exit"
        );
        println!(
            "TELEGRAM_TUI_ORDER_PRODUCTION original_cli_01561=true local_allow_remote_stale_deny=true remote_deny_tui_retired=true actual_pty_input=true same_thread_context=true final_only_after_native_completion=true simultaneous_race_tested=false"
        );
        return;
    }
    let user = PathBuf::from(std::env::var_os("HOME").unwrap());
    let home = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
    let root = user.parent().unwrap();
    let failure_line = root.join("fixture-failure-line");
    std::panic::set_hook(Box::new(move |info| {
        if let Some(location) = info.location()
            && location
                .file()
                .ends_with("telegram_tui_interaction_integration_tests.rs")
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
    tui_order_stage(root, "starting");
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
            .route("/v1/responses", post(tui_order_model))
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
    tui_order_stage(root, "source");
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
    tui_order_stage(root, "configure");
    owner(
        &app,
        &cookie,
        "configure",
        json!({"token":"101:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"}),
    )
    .await;
    tui_order_stage(root, "pair");
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
    tui_order_stage(root, "confirm");
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
    let mut update_id = 1;
    let mut stale_deny_update = None;
    let mut references = Vec::new();
    for turn in 1..=2usize {
        let case = if turn == 1 { "allowed" } else { "denied" };
        tui_order_stage(
            root,
            if turn == 1 {
                "first_turn"
            } else {
                "second_turn"
            },
        );
        fresh_poll(transport).await;
        transport.permit_final.store(false, Ordering::SeqCst);
        update_id += 1;
        let input_message = update_id;
        transport.updates.lock().unwrap().push_back(update(
            update_id,
            update_id,
            202,
            if turn == 1 {
                "SYNTHETIC_TUI_ALLOW"
            } else {
                "SYNTHETIC_REMOTE_DENY"
            },
        ));
        until(|| transport.sent.lock().unwrap().len() == turn * 2 - 1).await;
        let prompt = transport.sent.lock().unwrap()[turn * 2 - 2].clone();
        assert!(prompt.get("parse_mode").is_none());
        file_scope_prompt(prompt["text"].as_str().unwrap(), &cwd, case);
        let buttons: Vec<_> = prompt["reply_markup"]["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap())
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
        references.push(reference.clone());
        let message = (turn * 2 - 1) as i64;
        let task = status(&app, &cookie).await["channel"]["lastTask"]["id"].clone();
        // Poll boundaries allow transport progress but do not prove the first
        // prompt reference was installed as live authority before local input.
        // This direction qualifies rejection of the observed prompt's stale
        // callback, without claiming an exact activation-time barrier.
        fresh_poll(transport).await;
        fresh_poll(transport).await;
        tui_order_wait_menu(
            &terminal,
            &mut terminal_events,
            root,
            &cwd,
            case,
            if turn == 1 {
                "local_menu"
            } else {
                "remote_menu"
            },
        )
        .await;
        file_scope_files(&cwd, case, false);
        assert_eq!(transport.model.lock().unwrap().len(), turn * 2 - 1);
        if turn == 1 {
            // y is accepted only after the installed current screen explicitly
            // displays Yes, proceed (y) for this exact synthetic file scope.
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
            file_scope_files(&cwd, "allowed", true);
            assert_eq!(
                file_scope_receipts(&wire)
                    .iter()
                    .filter(|r| r["kind"] == "callback_received")
                    .count(),
                0
            );
            assert_eq!(wire.acknowledgments.load(Ordering::SeqCst), 0);
            assert!(!transport.permit_final.load(Ordering::SeqCst));
            let active_task = status(&app, &cookie).await["channel"]["lastTask"].clone();
            assert_eq!(active_task["id"], task);
            assert_eq!(active_task["nativeCompleted"], false);
            let before_stale = file_scope_receipts(&wire);
            assert!(
                before_stale
                    .iter()
                    .any(|r| r["kind"] == "submitting" && r["task"] == task)
            );
            assert!(
                !before_stale
                    .iter()
                    .any(|r| r["kind"] == "closed" && r["task"] == task)
            );
            update_id += 1;
            stale_deny_update = Some(update_id);
            transport
                .updates
                .lock()
                .unwrap()
                .push_back(interaction_callback(
                    update_id,
                    202,
                    message,
                    "callback_file_stale_after_tui",
                    &reference,
                ));
            until(|| {
                file_scope_receipts(&wire)
                    .iter()
                    .any(|r| r["kind"] == "rejected" && r["update_id"] == update_id)
            })
            .await;
            fresh_poll(transport).await;
            assert_eq!(
                file_scope_receipts(&wire)
                    .iter()
                    .filter(|r| r["kind"] == "callback_received")
                    .count(),
                0
            );
            assert_eq!(wire.acknowledgments.load(Ordering::SeqCst), 0);
            assert_eq!(transport.model.lock().unwrap().len(), 2);
            assert!(!transport.permit_final.load(Ordering::SeqCst));
            assert!(
                !file_scope_receipts(&wire)
                    .iter()
                    .any(|r| r["kind"] == "closed" && r["task"] == task)
            );
            file_scope_files(&cwd, "allowed", true);
        } else {
            update_id += 1;
            let callback_id = update_id;
            transport
                .updates
                .lock()
                .unwrap()
                .push_back(interaction_callback(
                    update_id,
                    202,
                    message,
                    "callback_file_remote_first_deny",
                    &reference,
                ));
            until(|| {
                transport.model.lock().unwrap().len() == 4
                    && wire.acknowledgments.load(Ordering::SeqCst) == 1
            })
            .await;
            let receipts = file_scope_receipts(&wire);
            let received = receipts
                .iter()
                .find(|r| r["kind"] == "received" && r["task"] == task)
                .unwrap();
            assert_eq!(
                received["key"],
                json!({"bot_id":101,"chat_id":202,"message_id":input_message})
            );
            let callback = receipts
                .iter()
                .find(|r| r["kind"] == "callback_received" && r["update_id"] == callback_id)
                .unwrap();
            assert_eq!(callback["task"], task);
            assert_eq!(callback["bot_id"], 101);
            file_scope_files(&cwd, "denied", false);
            assert!(!transport.permit_final.load(Ordering::SeqCst));
            assert!(
                !receipts
                    .iter()
                    .any(|r| r["kind"] == "closed" && r["task"] == task)
            );
            // A remote decline has already produced the native rejected tool
            // result. Observe modal retirement; do not press a stale local key.
            tui_order_wait_retired(&terminal, &mut terminal_events, root, &cwd, case).await;
            assert_eq!(input_sequence, onboarding_sequence + 1);
        }
        assert_eq!(transport.sent.lock().unwrap().len(), turn * 2 - 1);
        assert_eq!(
            status(&app, &cookie).await["channel"]["lastTask"]["nativeCompleted"],
            false
        );
        transport.permit_final.store(true, Ordering::SeqCst);
        until(|| transport.sent.lock().unwrap().len() == turn * 2).await;
        let final_message = transport.sent.lock().unwrap()[turn * 2 - 1].clone();
        assert_eq!(
            final_message["text"],
            format!("SYNTHETIC_TUI_ORDER_FINAL_{turn}")
        );
        assert!(final_message.get("reply_markup").is_none());
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
    }
    assert_eq!(input_sequence, onboarding_sequence + 1);
    assert_eq!(wire.acknowledgments.load(Ordering::SeqCst), 1);
    let context = transport.model.lock().unwrap()[2]["input"].to_string();
    assert!(
        context.contains("SYNTHETIC_TUI_ALLOW")
            && context.contains("SYNTHETIC_TUI_ORDER_FINAL_1")
            && context.contains("SYNTHETIC_REMOTE_DENY")
    );
    let receipts = file_scope_receipts(&wire);
    for kind in ["received", "submitting", "closed"] {
        assert_eq!(receipts.iter().filter(|r| r["kind"] == kind).count(), 2);
    }
    assert_eq!(
        receipts
            .iter()
            .filter(|r| r["kind"] == "callback_received")
            .count(),
        1
    );
    assert_eq!(
        receipts
            .iter()
            .filter(|r| r["kind"] == "rejected" && r["update_id"] == stale_deny_update.unwrap())
            .count(),
        1
    );
    let raw = std::fs::read_to_string(&wire.journal).unwrap();
    assert!(!raw.contains(thread.as_str().unwrap()));
    for reference in references {
        assert!(!raw.contains(&reference));
    }
    file_scope_files(&cwd, "allowed", true);
    file_scope_files(&cwd, "denied", false);
    tui_order_stage(root, "shutdown");
    drop(terminal_events);
    drop(app);
    tui_order_stage(root, "passed");
    std::fs::write(root.join("passed"), b"passed").unwrap();
}
