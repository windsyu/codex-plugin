// Included in telegram_native_integration. Native CLI, Application, receipt and
// interaction actors are real; model and Telegram peers are isolated loopback fixtures.
const FILE_OLD_UPDATE: &[u8] = b"SYNTHETIC_OLD_UPDATE\n";
const FILE_OLD_DELETE: &[u8] = b"SYNTHETIC_OLD_DELETE\n";
const FILE_OLD_MOVE: &[u8] = b"SYNTHETIC_OLD_MOVE\n";
struct FileScopeWire {
    transport: Arc<Wire>,
    journal: PathBuf,
    acknowledgments: AtomicUsize,
    cwd: PathBuf,
}
fn file_scope_seed(cwd: &Path) {
    for case in ["denied", "allowed"] {
        let dir = cwd.join(case);
        std::fs::create_dir(&dir).unwrap();
        for (name, bytes) in [
            ("fixture-update.txt", FILE_OLD_UPDATE),
            ("fixture-delete.txt", FILE_OLD_DELETE),
            ("fixture-source.txt", FILE_OLD_MOVE),
        ] {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        file_scope_files(cwd, case, false);
    }
}
fn file_scope_files(cwd: &Path, case: &str, applied: bool) {
    let dir = cwd.join(case);
    let expected: std::collections::BTreeMap<&str, &[u8]> = if applied {
        [
            ("fixture-add.txt", b"SYNTHETIC_ADDED\n".as_slice()),
            ("fixture-update.txt", b"SYNTHETIC_NEW_UPDATE\n".as_slice()),
            ("fixture-target.txt", b"SYNTHETIC_NEW_MOVE\n".as_slice()),
        ]
        .into_iter()
        .collect()
    } else {
        [
            ("fixture-update.txt", FILE_OLD_UPDATE),
            ("fixture-delete.txt", FILE_OLD_DELETE),
            ("fixture-source.txt", FILE_OLD_MOVE),
        ]
        .into_iter()
        .collect()
    };
    let actual: std::collections::BTreeSet<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            assert!(
                entry.file_type().unwrap().is_file(),
                "fixture directory must contain only ordinary files"
            );
            entry.file_name().into_string().unwrap()
        })
        .collect();
    assert!(
        actual == expected.keys().map(|name| (*name).to_owned()).collect(),
        "fixture file set mismatch"
    );
    for (name, bytes) in expected {
        assert!(
            std::fs::read(dir.join(name)).unwrap() == bytes,
            "fixture file bytes mismatch"
        );
    }
}
fn file_scope_receipts(wire: &FileScopeWire) -> Vec<Value> {
    let text = std::fs::read_to_string(&wire.journal).unwrap();
    for private in [
        "SYNTHETIC_",
        "fixture-",
        "callback_file_",
        "call_file_scope_",
        "file_scope_response_",
        "*** Begin Patch",
        "apply_patch",
        "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    ] {
        assert!(
            !text.contains(private),
            "private file content entered receipts"
        );
    }
    assert!(!text.contains(wire.cwd.to_str().unwrap()));
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
            row["record"]["event"].clone()
        })
        .collect()
}
async fn file_scope_bot(
    State(wire): State<Arc<FileScopeWire>>,
    uri: Uri,
    body: Bytes,
) -> axum::Json<Value> {
    if uri.path().ends_with("/answerCallbackQuery") {
        let request: Value = serde_json::from_slice(&body).unwrap();
        let digest = blake3::hash(request["callback_query_id"].as_str().unwrap().as_bytes())
            .to_hex()
            .to_string();
        assert_eq!(
            file_scope_receipts(&wire)
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
fn file_scope_patch(case: &str) -> String {
    format!(
        "apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: {case}/fixture-add.txt\n+SYNTHETIC_ADDED\n*** Update File: {case}/fixture-update.txt\n@@\n-SYNTHETIC_OLD_UPDATE\n+SYNTHETIC_NEW_UPDATE\n*** Delete File: {case}/fixture-delete.txt\n*** Update File: {case}/fixture-source.txt\n*** Move to: {case}/fixture-target.txt\n@@\n-SYNTHETIC_OLD_MOVE\n+SYNTHETIC_NEW_MOVE\n*** End Patch\nPATCH"
    )
}
async fn file_scope_model(State(wire): State<Arc<FileScopeWire>>, body: Bytes) -> Response {
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
                [("SYNTHETIC_FILE_DENY", 1), ("SYNTHETIC_FILE_ALLOW", 2)]
                    .into_iter()
                    .find_map(|(marker, turn)| content.contains(marker).then_some(turn))
            })
            .expect("known synthetic file turn");
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
                turn
            );
            let text = output["output"].to_string();
            if turn == 1 {
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
                if turn == 1 { "denied" } else { "allowed" },
                turn == 2,
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
            let args = json!({"cmd":file_scope_patch(if turn == 1 { "denied" } else { "allowed" }),"login":false,"yield_time_ms":1000});
            json!({"id":format!("item_file_{sequence}"),"type":"function_call","call_id":format!("call_file_scope_{turn}"),"name":"exec_command","arguments":args.to_string()})
        } else {
            let text = if title { r#"{"title":"Synthetic file scope"}"#.to_owned() } else { format!("SYNTHETIC_FILE_FINAL_{turn}") };
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
fn file_scope_prompt(text: &str, cwd: &Path, case: &str) {
    assert!(text.starts_with("File approval — these changes only:\n"));
    assert!(text.encode_utf16().count() <= 4096);
    for private in ["SYNTHETIC_", "*** Begin Patch", "apply_patch", "cwd"] {
        assert!(
            !text.contains(private),
            "file prompt disclosed private patch content"
        );
    }
    let actual: std::collections::BTreeSet<_> = text
        .lines()
        .filter(|line| {
            line.starts_with("Add: ")
                || line.starts_with("Delete: ")
                || line.starts_with("Update: ")
                || line.starts_with("Update and move: ")
        })
        .map(str::to_owned)
        .collect();
    let expected = |absolute: bool| {
        let path = |name: &str| {
            if absolute {
                cwd.join(case).join(name).display().to_string()
            } else {
                format!("{case}/{name}")
            }
        };
        [
            format!("Add: {}", path("fixture-add.txt")),
            format!("Delete: {}", path("fixture-delete.txt")),
            format!("Update: {}", path("fixture-update.txt")),
            format!(
                "Update and move: {} → {}",
                path("fixture-source.txt"),
                path("fixture-target.txt")
            ),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
    };
    assert!(
        actual == expected(true) || actual == expected(false),
        "exact complete mixed file scope required"
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("Add: ")
                || line.starts_with("Delete: ")
                || line.starts_with("Update: ")
                || line.starts_with("Update and move: "))
            .count(),
        4
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit isolated original CLI 0.156.1; production mixed file approvals with synthetic peers"]
async fn installed_cli_production_telegram_file_scope_path() {
    const CHILD: &str = "CODEX_TELEGRAM_FILE_SCOPE_CHILD";
    const TEST: &str = "workbench::application::tests::telegram_native_integration::installed_cli_production_telegram_file_scope_path";
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
            println!("TELEGRAM_FILE_SCOPE_FAILED fixture_line={line}");
        }
        let safe_stage = STAGES
            .iter()
            .copied()
            .find(|allowed| *allowed == raw_stage)
            .unwrap_or("unknown");
        assert!(
            matches!(exit, Ok(Ok(status)) if status.success()),
            "isolated file scope path failed at stage {safe_stage}"
        );
        assert!(root.path().join("passed").exists());
        assert!(
            native.peer.gone() && native.wrapper.gone(),
            "primary lifecycle must finish before host exit"
        );
        println!(
            "TELEGRAM_FILE_SCOPE_PRODUCTION original_cli_01561=true same_thread=true mixed_four_operation_deny=true mixed_four_operation_allow=true exact_filesystem_results=true complete_safe_scope=true receipt_before_continuation=true stale_and_wrong_identity_excluded=true final_only_after_native_completion=true"
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
                .ends_with("telegram_file_integration_tests.rs")
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
            .route("/v1/responses", post(file_scope_model))
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
    let mut references = Vec::new();
    for turn in 1..=2usize {
        let case = if turn == 1 { "denied" } else { "allowed" };
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
                "SYNTHETIC_FILE_DENY"
            } else {
                "SYNTHETIC_FILE_ALLOW"
            },
        ));
        until(|| transport.sent.lock().unwrap().len() == turn * 2 - 1).await;
        let prompt = transport.sent.lock().unwrap()[turn * 2 - 2].clone();
        assert!(prompt.get("parse_mode").is_none());
        file_scope_prompt(prompt["text"].as_str().unwrap(), &cwd, case);
        file_scope_files(&cwd, "denied", false);
        file_scope_files(&cwd, "allowed", false);
        let buttons: Vec<_> = prompt["reply_markup"]["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap())
            .collect();
        assert_eq!(buttons.len(), 2);
        let labels: std::collections::BTreeSet<_> = buttons
            .iter()
            .map(|b| b["text"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["Allow once", "Deny"].into_iter().collect());
        let reference = buttons
            .iter()
            .find(|b| b["text"] == if turn == 1 { "Deny" } else { "Allow once" })
            .unwrap()["callback_data"]
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
        // Bot send recording precedes its HTTP reply; allow confirmation and
        // reference activation to complete before delivering callback updates.
        fresh_poll(transport).await;
        fresh_poll(transport).await;
        update_id += 1;
        transport
            .updates
            .lock()
            .unwrap()
            .push_back(interaction_callback(
                update_id,
                999,
                message,
                "callback_file_wrong_user",
                &reference,
            ));
        update_id += 1;
        transport
            .updates
            .lock()
            .unwrap()
            .push_back(interaction_callback(
                update_id,
                202,
                99,
                "callback_file_wrong_message",
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
                    "callback_file_stale",
                    prior,
                ));
        }
        fresh_poll(transport).await;
        fresh_poll(transport).await;
        assert_eq!(transport.model.lock().unwrap().len(), turn * 2 - 1);
        assert_eq!(wire.acknowledgments.load(Ordering::SeqCst), turn - 1);
        assert_eq!(
            file_scope_receipts(&wire)
                .iter()
                .filter(|r| r["kind"] == "callback_received")
                .count(),
            turn - 1
        );
        file_scope_files(&cwd, case, false);
        update_id += 1;
        let callback_update = update_id;
        let query = format!("callback_file_accepted_{turn}");
        transport
            .updates
            .lock()
            .unwrap()
            .push_back(interaction_callback(
                update_id, 202, message, &query, &reference,
            ));
        until(|| {
            transport.model.lock().unwrap().len() == turn * 2
                && wire.acknowledgments.load(Ordering::SeqCst) == turn
        })
        .await;
        let receipts = file_scope_receipts(&wire);
        let receipt = receipts
            .iter()
            .find(|r| r["kind"] == "callback_received" && r["update_id"] == callback_update)
            .unwrap();
        assert_eq!(receipt["task"], task);
        assert_eq!(receipt["bot_id"], 101);
        let received = receipts
            .iter()
            .find(|r| r["kind"] == "received" && r["task"] == task)
            .unwrap();
        assert_eq!(
            received["key"],
            json!({"bot_id":101,"chat_id":202,"message_id":input_message})
        );
        assert_eq!(transport.sent.lock().unwrap().len(), turn * 2 - 1);
        assert_eq!(
            status(&app, &cookie).await["channel"]["lastTask"]["nativeCompleted"],
            false
        );
        file_scope_files(&cwd, "denied", false);
        file_scope_files(&cwd, "allowed", turn == 2);
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
        let final_message = transport.sent.lock().unwrap()[turn * 2 - 1].clone();
        assert_eq!(
            final_message["text"],
            format!("SYNTHETIC_FILE_FINAL_{turn}")
        );
        assert!(final_message.get("reply_markup").is_none());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = status(&app, &cookie).await;
                if state["channel"]["lastTask"]["nativeCompleted"] == true {
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
        until(|| {
            file_scope_receipts(&wire)
                .iter()
                .any(|r| r["kind"] == "closed" && r["task"] == task)
        })
        .await;
        prior_reference = Some(reference);
    }
    let context = transport.model.lock().unwrap()[2]["input"].to_string();
    assert!(
        context.contains("SYNTHETIC_FILE_DENY")
            && context.contains("SYNTHETIC_FILE_FINAL_1")
            && context.contains("SYNTHETIC_FILE_ALLOW")
    );
    let receipts = file_scope_receipts(&wire);
    for kind in ["received", "submitting", "callback_received", "closed"] {
        assert_eq!(receipts.iter().filter(|r| r["kind"] == kind).count(), 2);
    }
    let raw = std::fs::read_to_string(&wire.journal).unwrap();
    assert!(!raw.contains(thread.as_str().unwrap()));
    for reference in references {
        assert!(!raw.contains(&reference));
    }
    for request in transport.model.lock().unwrap().iter() {
        if let Some(turn_id) = request["metadata"]["turn_id"].as_str() {
            assert!(!raw.contains(turn_id));
        }
    }
    file_scope_files(&cwd, "denied", false);
    file_scope_files(&cwd, "allowed", true);
    stage(root, "shutdown");
    drop(app);
    stage(root, "passed");
    std::fs::write(root.join("passed"), b"passed").unwrap();
}
