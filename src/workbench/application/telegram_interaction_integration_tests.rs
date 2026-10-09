// Included in telegram_native_integration; shares only synthetic transport and
// the original fixture's kernel-identified native process cleanup guard.
struct InteractionWire {
    transport: Arc<Wire>,
    journal: PathBuf,
    acknowledgments: AtomicUsize,
}
fn interaction_receipts(wire: &InteractionWire) -> Vec<Value> {
    let bytes = std::fs::read(&wire.journal).unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    for private in [
        "SYNTHETIC_INTERACTION_",
        "SYNTHETIC_COMMAND_OUTPUT",
        "SYNTHETIC_PRIVATE_PATCH_BODY",
        "synthetic-denied.txt",
        "callback_query_",
        "Allow once",
        "apply_patch",
        "printf",
    ] {
        assert!(
            !text.contains(private),
            "private interaction content entered receipts"
        );
    }
    assert!(!text.contains(std::env::var("HOME").unwrap().as_str()));
    text.lines()
        .map(|line| {
            let row: Value = serde_json::from_str(line).unwrap();
            // Hash the original serialized record; parsing into Value changes field order.
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
            row["record"]["event"].clone()
        })
        .collect()
}
async fn interaction_bot(
    State(wire): State<Arc<InteractionWire>>,
    uri: Uri,
    body: Bytes,
) -> axum::Json<Value> {
    if uri.path().ends_with("/answerCallbackQuery") {
        let request: Value = serde_json::from_slice(&body).unwrap();
        let query = request["callback_query_id"].as_str().unwrap();
        let digest = blake3::hash(query.as_bytes()).to_hex().to_string();
        assert_eq!(
            interaction_receipts(&wire)
                .iter()
                .filter(|row| row["kind"] == "callback_received" && row["query_digest"] == digest)
                .count(),
            1
        );
        wire.acknowledgments.fetch_add(1, Ordering::SeqCst);
        return axum::Json(json!({"ok":true,"result":true}));
    }
    bot(State(wire.transport.clone()), uri, body).await
}
async fn interaction_model(State(wire): State<Arc<InteractionWire>>, body: Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap();
    let sequence = wire.transport.model_sequence.fetch_add(1, Ordering::SeqCst) + 1;
    assert!(sequence <= 12);
    let title = request["text"]["format"]["type"] == "json_schema"
        && request["text"]["format"]["schema"]["properties"]["title"]["type"] == "string"
        && request["text"]["format"]["schema"]["required"] == json!(["title"]);
    let (turn, tool) = if title {
        wire.transport.title_requests.fetch_add(1, Ordering::SeqCst);
        (0, false)
    } else {
        let input = request["input"].as_array().unwrap();
        let turn = input
            .iter()
            .rev()
            .filter(|item| item["role"] == "user")
            .find_map(|item| {
                let text = item["content"].to_string();
                [
                    ("SYNTHETIC_INTERACTION_COMMAND", 1),
                    ("SYNTHETIC_INTERACTION_FILE", 2),
                ]
                .into_iter()
                .find_map(|(marker, turn)| text.contains(marker).then_some(turn))
            })
            .expect("known synthetic interaction turn");
        let call = format!("call_interaction_{turn}");
        let output = input
            .iter()
            .find(|item| item["type"] == "function_call_output" && item["call_id"] == call);
        if let Some(output) = output {
            let receipts = interaction_receipts(&wire);
            assert_eq!(
                receipts
                    .iter()
                    .filter(|row| row["kind"] == "callback_received")
                    .count(),
                turn
            );
            let text = output["output"].to_string();
            if turn == 1 {
                assert!(
                    text.contains("SYNTHETIC_COMMAND_OUTPUT") && text.contains("code 0"),
                    "approved native command must succeed"
                );
            } else {
                assert!(
                    text.contains("rejected") || text.contains("declined"),
                    "native file operation must report rejection"
                );
            }
        }
        let tool = output.is_none();
        let mut requests = wire.transport.model.lock().unwrap();
        assert!(requests.len() < 4);
        requests.push(request);
        (turn, tool)
    };
    let stream = async_stream::stream! {
        let event = |v: Value| Bytes::from(format!("data: {v}\n\n"));
        let id = format!("interaction_response_{sequence}");
        yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":id,"model":"gpt-6-astra"}})));
        if !tool && !title {
            while !wire.transport.permit_final.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        let item = if tool {
            let arguments = if turn == 1 {
                json!({"cmd":"printf SYNTHETIC_COMMAND_OUTPUT","login":false,"sandbox_permissions":"require_escalated","justification":"Synthetic literal-output acceptance","yield_time_ms":1000})
            } else {
                json!({"cmd":"apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: synthetic-denied.txt\n+SYNTHETIC_PRIVATE_PATCH_BODY\n*** End Patch\nPATCH","login":false,"yield_time_ms":1000})
            };
            json!({"id":format!("interaction_item_{sequence}"),"type":"function_call","call_id":format!("call_interaction_{turn}"),"name":"exec_command","arguments":arguments.to_string()})
        } else {
            let text = if title { r#"{"title":"Synthetic interactions"}"#.to_owned() }
                else { format!("SYNTHETIC_INTERACTION_FINAL_{turn}") };
            json!({"id":format!("interaction_message_{sequence}"),"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":text}]})
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
fn interaction_callback(
    update: i64,
    user: i64,
    message: i64,
    query: &str,
    reference: &str,
) -> Value {
    json!({"update_id":update,"callback_query":{"id":query,"from":{"id":user,"is_bot":false},"message":{"message_id":message,"date":1,"from":{"id":101,"is_bot":true},"chat":{"id":202,"type":"private"}},"data":reference}})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit isolated original CLI 0.156.1; production Telegram interactions with synthetic peers"]
async fn installed_cli_production_telegram_interaction_path() {
    const CHILD: &str = "CODEX_TELEGRAM_INTERACTION_CHILD";
    const TEST: &str = "workbench::application::tests::telegram_native_integration::installed_cli_production_telegram_interaction_path";
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
            println!("TELEGRAM_INTERACTION_FAILED fixture_line={line}");
        }
        let safe_stage = STAGES
            .iter()
            .copied()
            .find(|allowed| *allowed == raw_stage)
            .unwrap_or("unknown");
        assert!(
            matches!(exit, Ok(Ok(status)) if status.success()),
            "isolated interaction path failed at stage {safe_stage}"
        );
        assert!(root.path().join("passed").exists());
        assert!(
            native.peer.gone() && native.wrapper.gone(),
            "primary lifecycle must finish before host exit"
        );
        println!(
            "TELEGRAM_INTERACTION_PRODUCTION original_cli_01561=true same_thread=true command_allow_once=true file_add_deny=true callback_receipts_before_continuation=true wrong_user_excluded=true stale_callback_excluded=true final_only_after_native_completion=true plain_bounded_prompts=true receipt_bodies_absent=true"
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
                .ends_with("telegram_interaction_integration_tests.rs")
        {
            let _ = std::fs::write(&failure_line, location.line().to_string());
        }
    }));
    stage(root, "starting");
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
    let wire = Arc::new(InteractionWire {
        transport: Arc::new(Wire::default()),
        journal: paths.root.join("channel/receipts.jsonl"),
        acknowledgments: AtomicUsize::new(0),
    });
    let transport = &wire.transport;
    let (bot_addr, _bot) = server(
        Router::new()
            .route("/{*path}", post(interaction_bot))
            .with_state(wire.clone()),
    )
    .await;
    let (model_addr, _model) = server(
        Router::new()
            .route("/v1/responses", post(interaction_model))
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
    stage(root, "source");
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
    stage(root, "configure");
    owner(
        &app,
        &cookie,
        "configure",
        json!({"token":"101:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"}),
    )
    .await;
    stage(root, "pair");
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
    stage(root, "confirm");
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
    let mut update_id = 1;
    let mut prior_reference = None::<String>;
    for turn in 1..=2usize {
        stage(
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
                "SYNTHETIC_INTERACTION_COMMAND"
            } else {
                "SYNTHETIC_INTERACTION_FILE"
            },
        ));
        until(|| transport.sent.lock().unwrap().len() == turn * 2 - 1).await;
        let prompt = transport.sent.lock().unwrap()[turn * 2 - 2].clone();
        let text = prompt["text"].as_str().unwrap();
        assert!(text.encode_utf16().count() <= 4096 && prompt.get("parse_mode").is_none());
        assert!(
            !text.contains("SYNTHETIC_PRIVATE_PATCH_BODY") && !text.contains("*** Begin Patch")
        );
        if turn == 1 {
            assert!(text.starts_with("Command approval — applies to this command only:\n"));
            assert!(text.contains("printf SYNTHETIC_COMMAND_OUTPUT"));
            assert!(
                !text.contains(cwd.to_str().unwrap()),
                "unrequested cwd must not be projected"
            );
        } else {
            // An explicit complete Add scope includes its target path; cwd and diff are not separate disclosures.
            assert!(text.starts_with("File approval — these changes only:\n"));
            assert_eq!(
                text.lines()
                    .filter(|line| line.starts_with("Add: "))
                    .count(),
                1
            );
            let add = text.lines().find(|line| line.starts_with("Add: ")).unwrap();
            assert!(
                add == format!("Add: {}", cwd.join("synthetic-denied.txt").display())
                    || add == "Add: synthetic-denied.txt",
                "file approval must describe precisely the synthetic target"
            );
            assert!(!text.contains("cwd"));
            assert!(!cwd.join("synthetic-denied.txt").exists());
        }
        let buttons = prompt["reply_markup"]["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap())
            .collect::<Vec<_>>();
        assert!(!buttons.is_empty() && buttons.len() <= 2);
        assert!(
            buttons
                .iter()
                .all(|b| matches!(b["text"].as_str(), Some("Allow once" | "Deny")))
        );
        if turn == 2 {
            assert_eq!(buttons.len(), 2);
        }
        let reference = buttons
            .iter()
            .find(|button| button["text"] == if turn == 1 { "Allow once" } else { "Deny" })
            .unwrap()["callback_data"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(reference.len(), 32);
        let message = (turn * 2 - 1) as i64;
        let task = status(&app, &cookie).await["channel"]["lastTask"]["id"].clone();
        assert_eq!(transport.model.lock().unwrap().len(), turn * 2 - 1);
        assert_eq!(
            status(&app, &cookie).await["channel"]["lastTask"]["nativeCompleted"],
            false
        );
        // Wrong user and stale prior-prompt references cannot release native work.
        update_id += 1;
        transport
            .updates
            .lock()
            .unwrap()
            .push_back(interaction_callback(
                update_id,
                999,
                message,
                "callback_query_wrong_user",
                &reference,
            ));
        if let Some(prior) = &prior_reference {
            update_id += 1;
            transport
                .updates
                .lock()
                .unwrap()
                .push_back(interaction_callback(
                    update_id,
                    202,
                    message,
                    "callback_query_stale",
                    prior,
                ));
        }
        fresh_poll(transport).await;
        fresh_poll(transport).await;
        assert_eq!(transport.model.lock().unwrap().len(), turn * 2 - 1);
        assert_eq!(wire.acknowledgments.load(Ordering::SeqCst), turn - 1);
        assert_eq!(
            interaction_receipts(&wire)
                .iter()
                .filter(|row| row["kind"] == "callback_received")
                .count(),
            turn - 1
        );
        update_id += 1;
        let callback_id = update_id;
        let query = format!("callback_query_accepted_{turn}");
        transport
            .updates
            .lock()
            .unwrap()
            .push_back(interaction_callback(
                callback_id,
                202,
                message,
                &query,
                &reference,
            ));
        until(|| {
            transport.model.lock().unwrap().len() == turn * 2
                && wire.acknowledgments.load(Ordering::SeqCst) == turn
        })
        .await;
        let receipts = interaction_receipts(&wire);
        let receipt = receipts
            .iter()
            .find(|row| row["kind"] == "callback_received" && row["update_id"] == callback_id)
            .unwrap();
        assert_eq!(receipt["task"], task);
        let source_receipt = receipts
            .iter()
            .find(|row| row["kind"] == "received" && row["task"] == task)
            .unwrap();
        assert_eq!(
            source_receipt["key"],
            json!({"bot_id":101,"chat_id":202,"message_id":input_message})
        );
        assert_eq!(receipt["bot_id"], 101);
        assert!(
            receipt["interaction"]
                .as_str()
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
        );
        // Receipt admission + native tool output precede native Turn completion.
        assert_eq!(transport.sent.lock().unwrap().len(), turn * 2 - 1);
        assert_eq!(
            status(&app, &cookie).await["channel"]["lastTask"]["nativeCompleted"],
            false
        );
        update_id += 1;
        transport
            .updates
            .lock()
            .unwrap()
            .push_back(interaction_callback(
                update_id, 202, message, &query, &reference,
            ));
        fresh_poll(transport).await;
        fresh_poll(transport).await;
        assert_eq!(wire.acknowledgments.load(Ordering::SeqCst), turn);
        transport.permit_final.store(true, Ordering::SeqCst);
        until(|| transport.sent.lock().unwrap().len() == turn * 2).await;
        stage(
            root,
            if turn == 1 {
                "first_final"
            } else {
                "second_final"
            },
        );
        let final_message = transport.sent.lock().unwrap()[turn * 2 - 1].clone();
        assert_eq!(
            final_message["text"],
            format!("SYNTHETIC_INTERACTION_FINAL_{turn}")
        );
        assert_eq!(final_message["chat_id"], 202);
        assert!(final_message.get("reply_markup").is_none());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if status(&app, &cookie).await["channel"]["lastTask"]["nativeCompleted"] == true {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
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
        assert!(!cwd.join("synthetic-denied.txt").exists());
        prior_reference = Some(reference);
    }
    let context = transport.model.lock().unwrap()[2]["input"].to_string();
    assert!(
        context.contains("SYNTHETIC_INTERACTION_COMMAND")
            && context.contains("SYNTHETIC_INTERACTION_FINAL_1")
            && context.contains("SYNTHETIC_INTERACTION_FILE")
    );
    assert_eq!(
        interaction_receipts(&wire)
            .iter()
            .filter(|row| row["kind"] == "callback_received")
            .count(),
        2
    );
    stage(root, "shutdown");
    drop(app);
    stage(root, "passed");
    std::fs::write(root.join("passed"), b"passed").unwrap();
}
