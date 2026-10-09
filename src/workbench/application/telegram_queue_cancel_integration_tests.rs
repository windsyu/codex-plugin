// Included in telegram_native_integration. Original CLI and production actors;
// only the Bot API and model are synthetic loopback peers.
const QUEUE_CANCEL_STAGES: &[&str] = &[
    "starting",
    "source",
    "local",
    "configure",
    "pair",
    "confirm",
    "waiting",
    "queued",
    "negative",
    "cancel",
    "slow_ack",
    "fresh",
    "slow_final_ack",
    "shutdown",
    "passed",
];
fn queue_cancel_stage(root: &Path, name: &'static str) {
    assert!(QUEUE_CANCEL_STAGES.contains(&name));
    std::fs::write(root.join("stage"), name).unwrap();
}
struct QueueCancelWire {
    transport: Arc<Wire>,
    journal: PathBuf,
    ack_started: AtomicUsize,
    permit_ack: AtomicBool,
    final_started: AtomicBool,
    permit_final_ack: AtomicBool,
}
fn queue_cancel_receipts(wire: &QueueCancelWire) -> Vec<Value> {
    let text = std::fs::read_to_string(&wire.journal).unwrap();
    for forbidden in [
        "SYNTHETIC_",
        "/cancel",
        "synthetic-only",
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    ] {
        assert!(
            !text.contains(forbidden),
            "private content entered receipts"
        );
    }
    assert!(!text.contains(std::env::var("HOME").unwrap().as_str()));
    text.lines()
        .map(|line| {
            let row: Value = serde_json::from_str(line).unwrap();
            let record = line
                .strip_prefix("{\"record\":")
                .unwrap()
                .rsplit_once(",\"digest\":")
                .unwrap()
                .0;
            assert_eq!(
                row["digest"],
                blake3::hash(record.as_bytes()).to_hex().to_string()
            );
            if row["record"]["event"]["kind"] == "queued_canceled" {
                assert_eq!(row["record"]["version"], 5);
            }
            row["record"]["event"].clone()
        })
        .collect()
}
fn queue_cancel_command(request: &Value) -> Option<String> {
    let text = request["text"].as_str()?;
    let mut commands = text.lines().filter(|line| line.starts_with("/cancel "));
    let command = commands.next()?;
    assert!(commands.next().is_none());
    let reference = command.strip_prefix("/cancel ").unwrap();
    assert!(
        reference.len() == 32
            && reference
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(
        text,
        format!(
            "Task is waiting. Cancel before submission with:\n{command}\nAfter submission starts, cancellation is unavailable."
        )
    );
    Some(command.to_owned())
}
async fn queue_cancel_bot(
    State(wire): State<Arc<QueueCancelWire>>,
    uri: Uri,
    body: Bytes,
) -> axum::Json<Value> {
    if uri.path().ends_with("/sendMessage") {
        let request: Value = serde_json::from_slice(&body).unwrap();
        let text = request["text"].as_str().unwrap();
        assert!(text.len() < 1024);
        assert!(request.get("parse_mode").is_none());
        if queue_cancel_command(&request).is_none() && text != "SYNTHETIC_FINAL_1" {
            assert_eq!(text, "Queued task canceled before native submission.");
            let ordinal = wire.ack_started.fetch_add(1, Ordering::SeqCst) + 1;
            assert!(ordinal <= 3);
            // The receipt worker has completed append+fsync before this HTTP
            // request can arrive. The observer checks the corresponding record.
            assert!(
                queue_cancel_receipts(&wire)
                    .iter()
                    .filter(|r| r["kind"] == "queued_canceled")
                    .count()
                    >= ordinal
            );
            while !wire.permit_ack.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        if text == "SYNTHETIC_FINAL_1" {
            assert!(!wire.final_started.swap(true, Ordering::SeqCst));
            // Hold only the final delivery response; the independent notice
            // worker must remain usable while the execution actor waits here.
            while !wire.permit_final_ack.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        assert!(text == "SYNTHETIC_FINAL_1" || !text.contains("SYNTHETIC_"));
    }
    bot(State(wire.transport.clone()), uri, body).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit isolated original CLI 0.156.1; production queue cancellation with synthetic peers"]
async fn installed_cli_production_telegram_queue_cancel_path() {
    const CHILD: &str = "CODEX_TELEGRAM_QUEUE_CANCEL_CHILD";
    const TEST: &str = "workbench::application::tests::telegram_native_integration::installed_cli_production_telegram_queue_cancel_path";
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
            println!("TELEGRAM_QUEUE_CANCEL_FAILED fixture_line={line}");
        }
        let safe_stage = QUEUE_CANCEL_STAGES
            .iter()
            .copied()
            .find(|allowed| *allowed == raw_stage)
            .unwrap_or("unknown");
        assert!(
            matches!(exit, Ok(Ok(status)) if status.success()),
            "isolated queue cancellation path failed at stage {safe_stage}"
        );
        assert!(root.path().join("passed").exists());
        assert!(
            native.peer.gone() && native.wrapper.gone(),
            "primary lifecycle must finish before host exit"
        );
        println!(
            "TELEGRAM_QUEUE_CANCEL_PRODUCTION original_cli_01561=true canceled_waiting_and_queued=true cancel_receipts_before_ack=true invalid_commands_excluded=true native_busy_preserved=true slow_ack_native_and_poll_progress=true local_final_excluded=true subsequent_channel_task=true idle_native_slow_final_queued_cancel=true"
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
                .ends_with("telegram_queue_cancel_integration_tests.rs")
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
    queue_cancel_stage(root, "starting");
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
    let wire = Arc::new(Wire::default());
    let paths = WorkbenchPaths::from_user_home(&user).unwrap();
    let queue_wire = Arc::new(QueueCancelWire {
        transport: wire.clone(),
        journal: paths.root.join("channel/receipts.jsonl"),
        ack_started: AtomicUsize::new(0),
        permit_ack: AtomicBool::new(false),
        final_started: AtomicBool::new(false),
        permit_final_ack: AtomicBool::new(false),
    });

    let (bot_addr, _bot) = server(
        Router::new()
            .route("/{*path}", post(queue_cancel_bot))
            .with_state(queue_wire.clone()),
    )
    .await;
    let (model_addr, _model) = server(
        Router::new()
            .route("/v1/responses", post(model))
            .with_state(wire.clone()),
    )
    .await;
    std::fs::create_dir(home.join("app-server-daemon")).unwrap();
    std::fs::write(
        home.join("app-server-daemon/settings.json"),
        r#"{"remoteControlEnabled":false,"updater":{"autoUpdateEnabled":false}}"#,
    )
    .unwrap();
    std::fs::write(home.join("config.toml"),format!("model=\"synthetic-model\"\nmodel_provider=\"custom\"\ncheck_for_update_on_startup=false\n[features]\ndaemon_auto_start=false\n[model_providers.custom]\nname=\"Synthetic integration\"\nbase_url=\"http://{model_addr}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nexperimental_bearer_token=\"synthetic-only\"\n[tools.update_plan]\nenabled=true\n[projects.{}]\ntrust_level=\"trusted\"\n",toml::Value::String(cwd.display().to_string()))).unwrap();
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
        panic!("isolated owner required")
    };
    let app = Application::start(prepared, home, paths, lock, None).unwrap();
    app.web.handle().telegram_api_for_test(bot_addr);
    app._runs
        .handle
        .launch(Connect {
            project: Some(cwd),
            ..Default::default()
        })
        .await
        .unwrap();
    let cookie = pair(&app).await;
    queue_cancel_stage(root, "source");
    let source = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let value = sources(&app, &cookie).await;
            if let Some(run) = value["runs"].as_array().and_then(|runs| {
                runs.iter().find(|run| {
                    run["source"]["targets"]
                        .as_array()
                        .is_some_and(|v| !v.is_empty())
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
    until(|| root.join("guarded").exists()).await;
    queue_cancel_stage(root, "local");
    let (tx, rx) = oneshot::channel();
    app._runs
        .handle
        .sender
        .send(Request::Inspect(Box::new(move |runner| {
            assert!(tx.send(runner.active[0].0.terminal()).is_ok());
        })))
        .unwrap();
    let terminal = rx.await.unwrap();
    let page = terminal.attach().await.unwrap();
    let grant = terminal.claim(page.connection_id).await.unwrap();
    let mut sequence = 0;
    let mut themed = false;
    let mut trusted = false;
    let mut ready = None;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let view = terminal.attach().await.unwrap();
            let mut parser = vt100::Parser::new(view.snapshot.rows, view.snapshot.cols, 0);
            parser.process(&view.snapshot.screen);
            parser.process(&view.snapshot.replay);
            let contents = parser.screen().contents();
            let theme =
                contents.contains("Choose your style") || contents.contains("Select a theme");
            let trust = contents.contains("Do you trust")
                || contents.contains("Do you want to work in this directory")
                || contents.contains("Trust this folder?");
            if (!themed && theme) || (!trusted && trust) {
                themed |= theme;
                trusted |= trust;
                tokio::time::sleep(Duration::from_millis(300)).await;
                sequence += 1;
                terminal
                    .input(
                        page.connection_id,
                        grant.generation,
                        sequence,
                        b"\r".to_vec(),
                    )
                    .await
                    .unwrap();
            } else if contents.contains("OpenAI Codex")
                && contents.contains('›')
                && ready.get_or_insert_with(std::time::Instant::now).elapsed()
                    > Duration::from_millis(300)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("isolated TUI composer readiness; raw screen suppressed");
    sequence += 1;
    terminal
        .input(
            page.connection_id,
            grant.generation,
            sequence,
            b"\x1b[200~SYNTHETIC_LOCAL_ONLY\x1b[201~".to_vec(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let view = terminal.attach().await.unwrap();
            let mut parser = vt100::Parser::new(view.snapshot.rows, view.snapshot.cols, 0);
            parser.process(&view.snapshot.screen);
            parser.process(&view.snapshot.replay);
            if parser.screen().contents().contains("SYNTHETIC_LOCAL_ONLY") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("synthetic local draft must render before Enter");
    sequence += 1;
    terminal
        .input(
            page.connection_id,
            grant.generation,
            sequence,
            b"\r".to_vec(),
        )
        .await
        .unwrap();
    until(|| wire.model.lock().unwrap().len() == 2).await;
    assert_eq!(
        sources(&app, &cookie).await["runs"][0]["source"]["targets"][0]["busy"],
        true
    );
    queue_cancel_stage(root, "configure");
    owner(
        &app,
        &cookie,
        "configure",
        json!({"token":"101:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"}),
    )
    .await;
    queue_cancel_stage(root, "pair");
    let pairing = owner(&app, &cookie, "pair", json!({})).await;
    fresh_poll(&wire).await;
    fresh_poll(&wire).await;
    wire.updates.lock().unwrap().push_back(update(
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
    let source = sources(&app, &cookie).await;
    let run = &source["runs"][0];
    queue_cancel_stage(root, "confirm");
    let confirmed = owner(&app,&cookie,"confirm",json!({"candidate":candidate,"runId":run["runId"],"threadId":thread,"sourceRevision":run["source"]["revision"]})).await;
    assert_eq!(
        confirmed["channel"]["binding"]["active"]["interactiveConsent"],
        false
    );
    fresh_poll(&wire).await;
    fresh_poll(&wire).await;
    queue_cancel_stage(root, "waiting");
    wire.updates
        .lock()
        .unwrap()
        .push_back(update(2, 2, 202, "SYNTHETIC_FIRST_CONTEXT"));
    until(|| wire.sent.lock().unwrap().len() == 1).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let s = status(&app, &cookie).await;
            if s["channel"]["lastTask"]["phase"] == "waiting" && s["channel"]["queued"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first task popped and waiting on busy native source");
    queue_cancel_stage(root, "queued");
    wire.updates
        .lock()
        .unwrap()
        .push_back(update(3, 3, 202, "SYNTHETIC_SECOND_CONTEXT"));
    until(|| wire.sent.lock().unwrap().len() == 2).await;
    fresh_poll(&wire).await;
    fresh_poll(&wire).await;
    assert_eq!(status(&app, &cookie).await["channel"]["queued"], 1);
    let mut commands: Vec<String> = wire
        .sent
        .lock()
        .unwrap()
        .iter()
        .map(|request| queue_cancel_command(request).unwrap())
        .collect();
    assert_ne!(commands[0], commands[1]);
    let mut admitted: Vec<Value> = queue_cancel_receipts(&queue_wire)
        .into_iter()
        .filter(|r| r["kind"] == "received")
        .collect();
    assert_eq!(admitted.len(), 2);
    assert_eq!(admitted[0]["key"]["message_id"], 2);
    assert_eq!(admitted[1]["key"]["message_id"], 3);

    queue_cancel_stage(root, "negative");
    let mut forwarded = update(7, 7, 202, &commands[0]);
    forwarded["message"]["forward_origin"] =
        json!({"type":"user","date":1,"sender_user":{"id":202,"is_bot":false}});
    wire.updates.lock().unwrap().extend([
        update(4, 4, 999, &commands[0]),
        update(5, 5, 202, "/cancel 00000000000000000000000000000000"),
        update(6, 6, 202, "/cancel malformed"),
        forwarded,
    ]);
    until(|| {
        queue_cancel_receipts(&queue_wire)
            .iter()
            .filter(|r| {
                r["kind"] == "rejected"
                    && r["update_id"]
                        .as_i64()
                        .is_some_and(|id| (4..=7).contains(&id))
            })
            .count()
            == 4
    })
    .await;
    assert_eq!(wire.model.lock().unwrap().len(), 2);
    assert_eq!(wire.sent.lock().unwrap().len(), 2);
    assert!(
        !queue_cancel_receipts(&queue_wire)
            .iter()
            .any(|r| r["kind"] == "submitting" || r["kind"] == "queued_canceled")
    );

    queue_cancel_stage(root, "cancel");
    wire.updates
        .lock()
        .unwrap()
        .push_back(update(8, 8, 202, &commands[1]));
    until(|| queue_wire.ack_started.load(Ordering::SeqCst) == 1).await;
    // Cancel the still-queued second task first, while the first task remains
    // popped and waiting. With its acknowledgment withheld, polling must still
    // cancel that popped task and reject stale references.
    wire.updates.lock().unwrap().extend([
        update(9, 9, 202, &commands[0]),
        update(10, 8, 202, &commands[1]), // Same message, fresh update ID.
        update(11, 11, 202, &commands[1]), // Consumed reference, new message.
    ]);
    until(|| {
        queue_cancel_receipts(&queue_wire)
            .iter()
            .filter(|r| r["kind"] == "queued_canceled")
            .count()
            == 2
    })
    .await;
    fresh_poll(&wire).await;
    fresh_poll(&wire).await;
    let records = queue_cancel_receipts(&queue_wire);
    let canceled: Vec<_> = records
        .iter()
        .filter(|r| r["kind"] == "queued_canceled")
        .collect();
    for (index, row) in canceled.iter().enumerate() {
        assert_eq!(row["task"], admitted[1 - index]["task"]);
        assert_eq!(row["update_id"], 8 + index as i64);
        assert_eq!(
            row["key"],
            json!({"bot_id":101,"chat_id":202,"message_id":8 + index as i64})
        );
        assert!(
            !records
                .iter()
                .any(|r| r["kind"] == "submitting" && r["task"] == row["task"])
        );
    }
    assert!(
        records
            .iter()
            .any(|r| r["kind"] == "rejected" && r["update_id"] == 11)
    );
    assert_eq!(
        records.iter().filter(|r| r["kind"] == "received").count(),
        2
    );
    assert_eq!(wire.model.lock().unwrap().len(), 2);
    assert_eq!(
        sources(&app, &cookie).await["runs"][0]["source"]["targets"][0]["busy"],
        true
    );

    queue_cancel_stage(root, "slow_ack");
    assert!(!queue_wire.permit_ack.load(Ordering::SeqCst));
    wire.permit_final.store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if sources(&app, &cookie).await["runs"][0]["source"]["targets"][0]["busy"] == false {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("slow cancellation acknowledgment must not block native completion");
    assert_eq!(wire.sent.lock().unwrap().len(), 2);
    assert_eq!(wire.model.lock().unwrap().len(), 2);
    queue_wire.permit_ack.store(true, Ordering::SeqCst);
    until(|| wire.sent.lock().unwrap().len() == 4).await;
    {
        let sent = wire.sent.lock().unwrap();
        assert_eq!(sent[2]["text"], sent[3]["text"]);
    }
    assert_eq!(status(&app, &cookie).await["channel"]["queued"], 0);

    queue_cancel_stage(root, "fresh");
    wire.updates
        .lock()
        .unwrap()
        .push_back(update(12, 12, 202, "SYNTHETIC_FIRST_CONTEXT"));
    until(|| queue_wire.final_started.load(Ordering::SeqCst)).await;
    assert_eq!(wire.sent.lock().unwrap().len(), 4);
    assert_eq!(wire.model.lock().unwrap().len(), 4);
    queue_cancel_stage(root, "slow_final_ack");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if sources(&app, &cookie).await["runs"][0]["source"]["targets"][0]["busy"] == false {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native idle while channel final delivery remains unconfirmed");
    assert_eq!(status(&app, &cookie).await["channel"]["queued"], 0);
    wire.updates
        .lock()
        .unwrap()
        .push_back(update(13, 13, 202, "SYNTHETIC_SECOND_CONTEXT"));
    until(|| wire.sent.lock().unwrap().len() == 5).await;
    let final_wait_command = queue_cancel_command(&wire.sent.lock().unwrap()[4]).unwrap();
    fresh_poll(&wire).await;
    fresh_poll(&wire).await;
    assert_eq!(status(&app, &cookie).await["channel"]["queued"], 1);
    assert_eq!(wire.model.lock().unwrap().len(), 4);
    wire.updates
        .lock()
        .unwrap()
        .push_back(update(14, 14, 202, &final_wait_command));
    until(|| wire.sent.lock().unwrap().len() == 6).await;
    assert_eq!(queue_wire.ack_started.load(Ordering::SeqCst), 3);
    assert!(!queue_wire.permit_final_ack.load(Ordering::SeqCst));
    let final_wait_records = queue_cancel_receipts(&queue_wire);
    let final_wait_task = final_wait_records
        .iter()
        .find(|r| r["kind"] == "received" && r["key"]["message_id"] == 13)
        .unwrap();
    let final_wait_cancel = final_wait_records
        .iter()
        .find(|r| r["kind"] == "queued_canceled" && r["update_id"] == 14)
        .unwrap();
    assert_eq!(final_wait_cancel["task"], final_wait_task["task"]);
    assert_eq!(
        final_wait_cancel["key"],
        json!({"bot_id":101,"chat_id":202,"message_id":14})
    );
    assert!(
        !final_wait_records
            .iter()
            .any(|r| r["kind"] == "submitting" && r["task"] == final_wait_task["task"])
    );
    admitted.push(final_wait_task.clone());
    commands.push(final_wait_command);
    queue_wire.permit_final_ack.store(true, Ordering::SeqCst);
    until(|| wire.sent.lock().unwrap().len() == 7).await;
    assert_eq!(
        wire.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["text"] == "SYNTHETIC_FINAL_1")
            .count(),
        1
    );
    assert_eq!(wire.model.lock().unwrap().len(), 4);
    let context = wire.model.lock().unwrap()[2]["input"].to_string();
    assert!(context.contains("SYNTHETIC_LOCAL_ONLY") && context.contains("SYNTHETIC_FINAL_3"));
    assert!(!context.contains("SYNTHETIC_SECOND_CONTEXT") && !context.contains("/cancel"));
    let s = status(&app, &cookie).await;
    assert!(s["channel"]["binding"]["active"].is_object());
    assert_eq!(
        s["channel"]["binding"]["active"]["interactiveConsent"],
        false
    );
    // Bot acceptance precedes the asynchronous job close. Wait for that final
    // append before taking a stable replay snapshot of the completed workload.
    until(|| {
        let records = queue_cancel_receipts(&queue_wire);
        let fresh = records
            .iter()
            .find(|r| r["kind"] == "received" && r["key"]["message_id"] == 12);
        fresh.is_some_and(|fresh| {
            records
                .iter()
                .any(|r| r["kind"] == "closed" && r["task"] == fresh["task"])
        })
    })
    .await;
    let records = queue_cancel_receipts(&queue_wire);
    assert_eq!(
        records.iter().filter(|r| r["kind"] == "received").count(),
        4
    );
    assert_eq!(
        records
            .iter()
            .filter(|r| r["kind"] == "queued_canceled")
            .count(),
        3
    );
    assert_eq!(
        records.iter().filter(|r| r["kind"] == "submitting").count(),
        1
    );
    let raw = std::fs::read_to_string(&queue_wire.journal).unwrap();
    for command in &commands {
        assert!(!raw.contains(command.strip_prefix("/cancel ").unwrap()));
    }
    assert!(!raw.contains(thread.as_str().unwrap()));
    // Replay a private snapshot with the production reader; all atomic cancel
    // events must restore Closed rather than Received or Submitting.
    // Store requires its parent to be private too; the isolated user directory
    // is explicitly 0700, unlike the outer temporary fixture directory.
    let replay = user.join("receipt-replay");
    std::fs::create_dir(&replay).unwrap();
    std::fs::set_permissions(&replay, std::fs::Permissions::from_mode(0o700)).unwrap();
    // Initialize the real lock/journal pair before replacing the private
    // journal snapshot. A journal with no lock correctly fails closed.
    drop(crate::workbench::channel::store::Store::open(&replay).unwrap());
    std::fs::write(replay.join("receipts.jsonl"), raw).unwrap();
    std::fs::set_permissions(
        replay.join("receipts.jsonl"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut recovered = crate::workbench::channel::store::Store::open(&replay).unwrap();
    let states = recovered.recover().unwrap();
    for original in &admitted {
        let id: uuid::Uuid = serde_json::from_value(original["task"].clone()).unwrap();
        assert!(states.contains(&(id, crate::workbench::channel::store::Status::Closed)));
    }
    drop(recovered);
    queue_cancel_stage(root, "shutdown");
    drop(app);
    queue_cancel_stage(root, "passed");
    std::fs::write(root.join("passed"), b"passed").unwrap();
}
