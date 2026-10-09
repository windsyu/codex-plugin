// Original CLI + production Application/Service, with loopback synthetic peers only.
#[cfg(target_os = "macos")]
mod telegram_native_integration {
    use super::*;
    use axum::{
        Router,
        body::{Body, Bytes},
        extract::State,
        http::Uri,
        response::Response,
        routing::post,
    };
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    #[derive(Default)]
    struct Wire {
        updates: Mutex<VecDeque<Value>>,
        invalid_once: Mutex<Option<Value>>,
        sent: Mutex<Vec<Value>>,
        // Only ordinary conversation requests; title generation is independent.
        model: Mutex<Vec<Value>>,
        model_sequence: AtomicUsize,
        title_requests: AtomicUsize,
        model_diagnostic: Mutex<Option<PathBuf>>,
        polls: AtomicUsize,
        completed_polls: AtomicUsize,
        permit_final: AtomicBool,
    }
    struct Server(tokio::task::JoinHandle<()>);
    impl Drop for Server {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    async fn server(router: Router) -> (std::net::SocketAddr, Server) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (
            addr,
            Server(tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            })),
        )
    }
    async fn bot(State(wire): State<Arc<Wire>>, uri: Uri, body: Bytes) -> axum::Json<Value> {
        let request: Value = serde_json::from_slice(&body).unwrap();
        let method = uri.path().rsplit('/').next().unwrap();
        let result = match method {
            "getMe" => json!({"id":101,"is_bot":true}),
            "getWebhookInfo" => json!({"url":""}),
            "getUpdates" => {
                let ticket = wire.polls.fetch_add(1, Ordering::SeqCst) + 1;
                let values: Vec<_> = if let Some(invalid) = wire.invalid_once.lock().unwrap().take()
                {
                    vec![invalid]
                } else {
                    let mut updates = wire.updates.lock().unwrap();
                    if let Some(offset) = request["offset"].as_i64() {
                        updates.retain(|update| update["update_id"].as_i64().unwrap() >= offset);
                    }
                    updates.iter().take(20).cloned().collect()
                };
                tokio::time::sleep(Duration::from_millis(40)).await;
                wire.completed_polls.fetch_max(ticket, Ordering::SeqCst);
                json!(values)
            }
            "sendMessage" => {
                assert_eq!(request["chat_id"], 202);
                assert!(request.get("parse_mode").is_none());
                let mut sent = wire.sent.lock().unwrap();
                assert!(sent.len() < 8);
                sent.push(request);
                json!({"message_id":sent.len(),"chat":{"id":202,"type":"private"}})
            }
            _ => panic!("unexpected synthetic Bot API method"),
        };
        axum::Json(json!({"ok":true,"result":result}))
    }
    fn has_plan_tool(value: &Value) -> bool {
        match value {
            Value::Object(fields) => {
                fields.get("name").is_some_and(|v| v == "update_plan")
                    || fields.values().any(has_plan_tool)
            }
            Value::Array(values) => values.iter().any(has_plan_tool),
            _ => false,
        }
    }
    async fn model(State(wire): State<Arc<Wire>>, body: Bytes) -> Response {
        let request: Value = serde_json::from_slice(&body).unwrap();
        let sequence = wire.model_sequence.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(sequence <= 12);
        // The original TUI also creates ephemeral structured-output title threads.
        // Answer their public output schema without consuming a conversation turn.
        let title = request["text"]["format"]["type"] == "json_schema"
            && request["text"]["format"]["schema"]["properties"]["title"]["type"] == "string"
            && request["text"]["format"]["schema"]["required"] == json!(["title"]);
        if let Some(path) = wire.model_diagnostic.lock().unwrap().as_ref() {
            std::fs::write(path.with_file_name("model-checks"), json!({
                "title":title,"planTool":has_plan_tool(&request["tools"]),
                "inputArray":request["input"].is_array(),
                "toolsArray":request["tools"].is_array(),
                "planText":request["tools"].to_string().contains("update_plan"),
                "user":request["input"].as_array().is_some_and(|items| items.iter().any(|v| v["role"]=="user")),
                "jsonSchema":request["text"]["format"]["type"]=="json_schema"
            }).to_string()).unwrap();
        }
        let (turn, tool) = if title {
            wire.title_requests.fetch_add(1, Ordering::SeqCst);
            (0, false)
        } else {
            assert!(has_plan_tool(&request["tools"]));
            let input = request["input"].as_array().unwrap();
            // Full HTTP context contains earlier markers too. Choose the latest
            // actual user message, rather than the request's global ordinal.
            let turn = input
                .iter()
                .rev()
                .filter(|item| item["role"] == "user")
                .find_map(|item| {
                    let content = item["content"].to_string();
                    [
                        ("SYNTHETIC_LOCAL_ONLY", 3),
                        ("SYNTHETIC_SECOND_CONTEXT", 2),
                        ("SYNTHETIC_FIRST_CONTEXT", 1),
                    ]
                    .into_iter()
                    .find_map(|(marker, turn)| content.contains(marker).then_some(turn))
                })
                .expect("known synthetic user turn");
            let call_id = format!("call_context_{turn}");
            let output = input
                .iter()
                .find(|item| item["type"] == "function_call_output" && item["call_id"] == call_id);
            if let Some(output) = output {
                assert!(
                    output["output"].to_string().contains("Plan updated"),
                    "synthetic plan must execute successfully"
                );
            }
            let tool = output.is_none();
            let mut requests = wire.model.lock().unwrap();
            assert!(requests.len() < 6);
            requests.push(request);
            if let Some(path) = wire.model_diagnostic.lock().unwrap().as_ref() {
                let flags: Vec<_> = requests
                    .iter()
                    .map(|r| {
                        let input = r["input"].to_string();
                        json!({"first":input.contains("SYNTHETIC_FIRST_CONTEXT"),
                        "second":input.contains("SYNTHETIC_SECOND_CONTEXT"),
                        "local":input.contains("SYNTHETIC_LOCAL_ONLY"),
                        "planTool":has_plan_tool(&r["tools"]),
                        "previousResponseReference":r["previous_response_id"].is_string()})
                    })
                    .collect();
                std::fs::write(path, serde_json::to_vec(&flags).unwrap()).unwrap();
            }
            (turn, tool)
        };
        let stream = async_stream::stream! {
            let event = |v: Value| Bytes::from(format!("data: {v}\n\n"));
            let id = format!("response_synthetic_{sequence}");
            yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":id,"model":"synthetic-model"}})));
            if !tool && !title {
                while !wire.permit_final.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            let item = if tool {
                json!({"id":format!("item_{sequence}"),"type":"function_call","call_id":format!("call_context_{turn}"),"name":"update_plan","arguments":"{\"plan\":[{\"step\":\"Synthetic no-op\",\"status\":\"completed\"}]}"})
            } else {
                let text = if title { r#"{"title":"Synthetic title"}"#.to_owned() }
                    else { format!("SYNTHETIC_FINAL_{turn}") };
                json!({"id":format!("message_{sequence}"),"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":text}]})
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
    fn update(id: i64, message: i64, user: i64, text: &str) -> Value {
        json!({"update_id":id,"message":{"message_id":message,"date":1,"from":{"id":user,"is_bot":false},"chat":{"id":202,"type":"private"},"text":text}})
    }
    async fn until(mut predicate: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(20), async {
            while !predicate() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("synthetic integration deadline");
    }
    const STAGES: &[&str] = &[
        "starting",
        "source",
        "configure",
        "pair",
        "confirm",
        "first_turn",
        "second_turn",
        "first_final",
        "second_final",
        "first_thread",
        "second_thread",
        "context",
        "context_first_missing",
        "context_reply_missing",
        "context_second_missing",
        "dedup",
        "local",
        "revoke",
        "invalid_order",
        "shutdown",
        "passed",
    ];
    fn stage(root: &Path, name: &'static str) {
        assert!(STAGES.contains(&name));
        std::fs::write(root.join("stage"), name).unwrap();
    }
    async fn fresh_poll(wire: &Wire) {
        let before = wire.polls.load(Ordering::SeqCst);
        until(|| wire.polls.load(Ordering::SeqCst) > before).await;
        let entered = wire.polls.load(Ordering::SeqCst);
        until(|| wire.completed_polls.load(Ordering::SeqCst) >= entered).await;
    }
    async fn owner(app: &Application, cookie: &str, operation: &str, mut body: Value) -> Value {
        body["instanceId"] = json!(app.instance_id);
        let response = client()
            .post(format!(
                "{}/workbench/v1/application/telegram/{operation}",
                app.entry.address
            ))
            .header("Cookie", cookie)
            .header("Origin", &app.entry.address)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "owner mutation rejected");
        json_body(response).await
    }
    async fn status(app: &Application, cookie: &str) -> Value {
        json_body(get(app, cookie, "/workbench/v1/application/telegram").await).await
    }
    async fn sources(app: &Application, cookie: &str) -> Value {
        json_body(get(app, cookie, "/workbench/v1/application/channel-runs").await).await
    }

    struct ProcessIdentity {
        pid: i32,
        start: (u64, u64),
        group: u32,
    }
    impl ProcessIdentity {
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
                start: (info.pbi_start_tvsec, info.pbi_start_tvusec),
                group: info.pbi_pgid,
            })
        }
        fn same(&self) -> bool {
            Self::read(self.pid).is_some_and(|p| p.start == self.start && p.group == self.group)
        }
        fn gone(&self) -> bool {
            if let Some(p) = Self::read(self.pid) {
                return p.start != self.start;
            }
            (unsafe { libc::kill(self.pid, 0) }) == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        }
    }
    struct NativeGuard {
        peer: ProcessIdentity,
        wrapper: ProcessIdentity,
        leaf: PathBuf,
        inode: (u64, u64),
    }
    impl Drop for NativeGuard {
        fn drop(&mut self) {
            use std::os::unix::fs::MetadataExt;
            for process in [&self.peer, &self.wrapper] {
                if process.same() {
                    unsafe {
                        libc::kill(process.pid, libc::SIGKILL);
                    }
                }
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while (!self.peer.gone() || !self.wrapper.gone())
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            if self.peer.gone()
                && self.wrapper.gone()
                && std::fs::symlink_metadata(&self.leaf)
                    .is_ok_and(|m| (m.dev(), m.ino()) == self.inode)
            {
                let _ = std::fs::remove_file(&self.leaf);
            }
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "explicit isolated original CLI; synthetic loopback model and Bot API"]
    async fn installed_cli_production_telegram_path() {
        const CHILD: &str = "CODEX_TELEGRAM_INTEGRATION_CHILD";
        const TEST: &str = "workbench::application::tests::telegram_native_integration::installed_cli_production_telegram_path";
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
            let safe_stage = STAGES
                .iter()
                .copied()
                .find(|allowed| *allowed == raw_stage)
                .unwrap_or("unknown");
            if let Ok(bytes) = std::fs::read(root.path().join("context-flags"))
                && let Ok(flags) = serde_json::from_slice::<Value>(&bytes)
            {
                for key in ["first", "reply", "second", "previousResponseReference"] {
                    if let Some(value) = flags[key].as_bool() {
                        println!("TELEGRAM_CONTEXT {key}={value}");
                    }
                }
            }
            if let Ok(bytes) = std::fs::read(root.path().join("reply-flags"))
                && let Ok(flags) = serde_json::from_slice::<Value>(&bytes)
            {
                for key in ["exact", "containsFirst", "containsSecond", "containsThird"] {
                    if let Some(value) = flags[key].as_bool() {
                        println!("TELEGRAM_REPLY {key}={value}");
                    }
                }
                if let Some(length) = flags["bytes"].as_u64().filter(|n| *n <= 65536) {
                    println!("TELEGRAM_REPLY bytes={length}");
                }
            }
            if let Ok(bytes) = std::fs::read(root.path().join("model-checks"))
                && let Ok(flags) = serde_json::from_slice::<Value>(&bytes)
            {
                for key in [
                    "title",
                    "planTool",
                    "inputArray",
                    "toolsArray",
                    "planText",
                    "user",
                    "jsonSchema",
                ] {
                    if let Some(value) = flags[key].as_bool() {
                        println!("TELEGRAM_MODEL_CHECK {key}={value}");
                    }
                }
            }
            if let Ok(bytes) = std::fs::read(root.path().join("model-flags"))
                && let Ok(flags) = serde_json::from_slice::<Value>(&bytes)
                && let Some(rows) = flags.as_array().filter(|rows| rows.len() <= 12)
            {
                for (index, row) in rows.iter().enumerate() {
                    for key in [
                        "first",
                        "second",
                        "local",
                        "planTool",
                        "previousResponseReference",
                    ] {
                        if let Some(value) = row[key].as_bool() {
                            println!("TELEGRAM_MODEL request={} {key}={value}", index + 1);
                        }
                    }
                }
            }
            assert!(
                matches!(exit, Ok(Ok(status)) if status.success()),
                "isolated production path failed at stage {safe_stage}"
            );
            assert!(root.path().join("passed").exists());
            assert!(
                native.peer.gone() && native.wrapper.gone(),
                "primary lifecycle must finish before host exit"
            );
            println!(
                "TELEGRAM_PRODUCTION synthetic_peers=true original_cli=true background_title_exercised=true same_thread_context=true final_only=true message_key_duplicate_excluded=true invalid_update_order_failclosed=true wrong_user_excluded=true local_excluded=true revoked_excluded=true"
            );
            return;
        }
        let user = PathBuf::from(std::env::var_os("HOME").unwrap());
        let home = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
        let root = user.parent().unwrap();
        stage(root, "starting");
        let cwd = root.join("project");
        let wire = Arc::new(Wire::default());
        *wire.model_diagnostic.lock().unwrap() = Some(root.join("model-flags"));
        let (bot_addr, _bot) = server(
            Router::new()
                .route("/{*path}", post(bot))
                .with_state(wire.clone()),
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
        stage(root, "source");
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
        stage(root, "confirm");
        owner(&app,&cookie,"confirm",json!({"candidate":candidate,"runId":run["runId"],"threadId":thread,"sourceRevision":run["source"]["revision"]})).await;
        fresh_poll(&wire).await;
        fresh_poll(&wire).await;
        for turn in 1..=2usize {
            stage(
                root,
                if turn == 1 {
                    "first_turn"
                } else {
                    "second_turn"
                },
            );
            wire.permit_final.store(false, Ordering::SeqCst);
            wire.updates.lock().unwrap().push_back(update(
                turn as i64 + 1,
                turn as i64 + 1,
                202,
                if turn == 1 {
                    "SYNTHETIC_FIRST_CONTEXT"
                } else {
                    "SYNTHETIC_SECOND_CONTEXT"
                },
            ));
            until(|| wire.model.lock().unwrap().len() == turn * 2).await;
            // The tool's model response completed, but the native Turn has not.
            assert_eq!(wire.sent.lock().unwrap().len(), turn - 1);
            assert_eq!(
                status(&app, &cookie).await["channel"]["lastTask"]["nativeCompleted"],
                false
            );
            wire.permit_final.store(true, Ordering::SeqCst);
            until(|| wire.sent.lock().unwrap().len() == turn).await;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let s = status(&app, &cookie).await;
                    if s["channel"]["lastTask"]["nativeCompleted"] == true {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            stage(
                root,
                if turn == 1 {
                    "first_final"
                } else {
                    "second_final"
                },
            );
            {
                let sent = wire.sent.lock().unwrap();
                let text = sent[turn - 1]["text"].as_str().unwrap();
                std::fs::write(
                    root.join("reply-flags"),
                    json!({"exact":text==format!("SYNTHETIC_FINAL_{turn}"),
                    "containsFirst":text.contains("SYNTHETIC_FINAL_1"),
                    "containsSecond":text.contains("SYNTHETIC_FINAL_2"),
                    "containsThird":text.contains("SYNTHETIC_FINAL_3"),"bytes":text.len()})
                    .to_string(),
                )
                .unwrap();
                assert_eq!(sent[turn - 1]["text"], format!("SYNTHETIC_FINAL_{turn}"));
            }
            stage(
                root,
                if turn == 1 {
                    "first_thread"
                } else {
                    "second_thread"
                },
            );
            let current = sources(&app, &cookie).await;
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
        {
            let model = wire.model.lock().unwrap();
            stage(root, "context");
            let second = model[2]["input"].to_string();
            std::fs::write(root.join("context-flags"),json!({"first":second.contains("SYNTHETIC_FIRST_CONTEXT"),"reply":second.contains("SYNTHETIC_FINAL_1"),"second":second.contains("SYNTHETIC_SECOND_CONTEXT"),"previousResponseReference":model[2]["previous_response_id"].is_string()}).to_string()).unwrap();
            for (present, label) in [
                (
                    second.contains("SYNTHETIC_FIRST_CONTEXT"),
                    "context_first_missing",
                ),
                (
                    second.contains("SYNTHETIC_FINAL_1"),
                    "context_reply_missing",
                ),
                (
                    second.contains("SYNTHETIC_SECOND_CONTEXT"),
                    "context_second_missing",
                ),
            ] {
                if !present {
                    stage(root, label);
                }
                assert!(present, "same-thread native context component missing");
            }
            assert!(
                second.contains("SYNTHETIC_FIRST_CONTEXT")
                    && second.contains("SYNTHETIC_FINAL_1")
                    && second.contains("SYNTHETIC_SECOND_CONTEXT"),
                "same-thread native context must continue"
            );
        }
        assert!(
            wire.title_requests.load(Ordering::SeqCst) > 0,
            "original TUI title request must be exercised"
        );
        // Legal increasing update IDs exercise durable message-key dedup.
        stage(root, "dedup");
        wire.updates.lock().unwrap().extend([
            update(4, 2, 202, "SYNTHETIC_FIRST_CONTEXT"),
            update(5, 5, 999, "SYNTHETIC_WRONG_USER"),
        ]);
        let polls = wire.polls.load(Ordering::SeqCst);
        until(|| wire.polls.load(Ordering::SeqCst) > polls + 3).await;
        assert_eq!(wire.model.lock().unwrap().len(), 4);
        assert_eq!(wire.sent.lock().unwrap().len(), 2);
        let active = status(&app, &cookie).await;
        assert_eq!(active["channel"]["configured"], true);
        assert_eq!(active["channel"]["binding"]["phase"], "active");
        stage(root, "local");
        // A genuine local TUI turn must never become a Telegram response.
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
        until(|| wire.model.lock().unwrap().len() == 6).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let s = sources(&app, &cookie).await;
                if s["runs"][0]["source"]["targets"][0]["busy"] == false {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(wire.sent.lock().unwrap().len(), 2);
        stage(root, "revoke");
        owner(&app, &cookie, "revoke", json!({})).await;
        fresh_poll(&wire).await;
        wire.updates
            .lock()
            .unwrap()
            .push_back(update(6, 6, 202, "SYNTHETIC_REVOKED"));
        fresh_poll(&wire).await;
        fresh_poll(&wire).await;
        assert_eq!(wire.model.lock().unwrap().len(), 6);
        assert_eq!(wire.sent.lock().unwrap().len(), 2);
        stage(root, "invalid_order");
        *wire.invalid_once.lock().unwrap() = Some(update(2, 2, 202, "SYNTHETIC_FIRST_CONTEXT"));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let s = status(&app, &cookie).await;
                if s["channel"]["failure"] == "channel_update_order_invalid" {
                    assert_eq!(s["channel"]["configured"], false);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("invalid update order must fail closed");
        assert_eq!(wire.model.lock().unwrap().len(), 6);
        assert_eq!(wire.sent.lock().unwrap().len(), 2);
        stage(root, "shutdown");
        drop(app);
        stage(root, "passed");
        std::fs::write(root.join("passed"), b"passed").unwrap();
    }
}
