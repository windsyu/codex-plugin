mod r7_organization_integration {
    use super::*;
    use crate::workbench::terminal::TerminalHandle;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    async fn terminal(app: &Application, id: Uuid) -> TerminalHandle {
        let (tx, rx) = oneshot::channel();
        app._runs
            .handle
            .sender
            .send(Request::Inspect(Box::new(move |runner| {
                let handle = runner
                    .active
                    .iter()
                    .find(|(_, _, r)| r.run_id == id)
                    .unwrap()
                    .0
                    .terminal();
                let _ = tx.send(handle);
            })))
            .unwrap();
        rx.await.unwrap()
    }
    async fn entry_for(app: &Application, cookie: &str, thread: Uuid) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                launch_post(app, cookie, "/workbench/v1/library/refresh", json!({})).await;
                let body =
                    json_body(get(app, cookie, "/workbench/v1/library/entries?kind=native").await)
                        .await;
                if let Some(row) = body["records"]
                    .as_array()
                    .and_then(|r| r.iter().find(|r| r["nativeThreadId"] == thread.to_string()))
                {
                    break row.clone();
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn organize(
        app: &Application,
        cookie: &str,
        entry: &Value,
        action: &str,
        project: Option<&Value>,
        etag: &str,
    ) -> reqwest::Response {
        let mut body = json!({"instanceId":app.instance_id,"sourceIdentity":entry["sourceIdentity"],"sourceRevision":entry["sourceRevision"],"action":action});
        if let Some(project) = project {
            body["projectId"] = project.clone();
        }
        client()
            .put(format!(
                "{}/workbench/v1/library/entries/{}/organization",
                app.entry.address,
                entry["entryId"].as_str().unwrap()
            ))
            .header("Cookie", cookie)
            .header("Origin", &app.entry.address)
            .header("If-Match", etag)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap()
    }
    async fn run(app: &Application, cookie: &str, cwd: &Path) -> Value {
        let target = target_for(app, cookie, cwd).await;
        let body = start_body(app, &target);
        assert_eq!(
            launch_post(app, cookie, "/workbench/v1/runs", body.clone())
                .await
                .status(),
            202
        );
        let ready = operation_done(app, cookie, &body["operationId"]).await;
        assert_eq!(ready["state"], "ready", "{ready}");
        ready["run"].clone()
    }
    async fn consumed(path: &Path, expected: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if std::fs::read_to_string(path).is_ok_and(|text| text == expected) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual fake CLI must consume its scoped PTY input");
    }

    fn native_reply(index: usize, text: &str) -> axum::response::Response {
        let id = format!("r7-response-{index}");
        let item = format!("r7-item-{index}");
        let frames = [
            json!({"type":"response.created","response":{"id":id}}),
            json!({"type":"response.output_item.added","item":{"id":item,"type":"message","role":"assistant","content":[]}}),
            json!({"type":"response.output_text.delta","item_id":item,"content_index":0,"delta":text}),
            json!({"type":"response.output_item.done","item":{"id":item,"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}}),
            json!({"type":"response.completed","response":{"id":id}}),
        ];
        axum::response::Response::builder()
            .header("Content-Type", "text/event-stream")
            .body(axum::body::Body::from(
                frames
                    .iter()
                    .map(|v| format!("data: {v}\n\n"))
                    .collect::<String>(),
            ))
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "internal helper; invoke only through isolated launcher_home outer test"]
    async fn installed_resume_helper() {
        use crate::workbench::{terminal::TerminalEvent, test_native::NativeProbe};
        let isolated = PathBuf::from(
            std::env::var_os("R7_NATIVE_PROBE_ISOLATED_ROOT")
                .expect("isolated outer test required"),
        );
        assert_eq!(std::env::var_os("HOME").unwrap(), isolated.as_os_str());
        assert_eq!(
            std::env::var_os("USERPROFILE").unwrap(),
            isolated.as_os_str()
        );
        let fixture = Fixture::new();
        std::fs::create_dir(&fixture.home).unwrap();
        let other = fixture.dir.path().join("different-existing-project");
        std::fs::create_dir(&other).unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let thread = Arc::new(std::sync::Mutex::new(None::<Uuid>));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (counter, identity, original, wrong) = (
            seen.clone(),
            thread.clone(),
            fixture.cwd.canonicalize().unwrap(),
            other.canonicalize().unwrap(),
        );
        let router = axum::Router::new().fallback(move |request:axum::extract::Request| {
            let (counter,identity,original,wrong) = (counter.clone(),identity.clone(),original.clone(),wrong.clone());
            async move {
                assert_eq!(request.headers()["authorization"],"Bearer synthetic-r7-native");
                let body:Value = serde_json::from_slice(&axum::body::to_bytes(request.into_body(),2*1024*1024).await.unwrap()).unwrap();
                if body.pointer("/text/format/schema/properties/title").is_some() { return native_reply(99,"{\"title\":\"Synthetic R7\"}"); }
                let meta:Value = serde_json::from_str(body["client_metadata"]["x-codex-turn-metadata"].as_str().unwrap()).unwrap();
                let id:Uuid = meta["thread_id"].as_str().unwrap().parse().unwrap();
                let index = counter.fetch_add(1,Ordering::SeqCst);
                match index {
                    0 => { assert!(body["input"].to_string().contains("R7_SEED")); *identity.lock().unwrap()=Some(id); native_reply(0,"R7_SEED_REPLY") }
                    1 => {
                        assert_eq!(*identity.lock().unwrap(),Some(id));
                        assert!(body["input"].to_string().contains("R7_NEXT"));
                        let args=json!({"cmd":"pwd","login":false,"max_output_tokens":300,"yield_time_ms":1000}).to_string();
                        let tools=body["tools"].to_string();
                        let call=if tools.contains("\"exec\"") {json!({"type":"custom_tool_call","id":"r7-call","call_id":"r7-pwd","name":"exec","input":format!("text((await tools.exec_command({args})).output);")})} else {json!({"type":"function_call","id":"r7-call","call_id":"r7-pwd","name":"exec_command","arguments":args})};
                        let frames=[json!({"type":"response.created","response":{"id":"r7-tool"}}),json!({"type":"response.output_item.done","output_index":0,"item":call}),json!({"type":"response.completed","response":{"id":"r7-tool"}})];
                        axum::response::Response::builder().header("Content-Type","text/event-stream").body(axum::body::Body::from(frames.iter().map(|v|format!("data: {v}\n\n")).collect::<String>())).unwrap()
                    }
                    2 => {
                        assert_eq!(*identity.lock().unwrap(),Some(id));
                        let output=body["input"].as_array().unwrap().iter().find(|v|v["call_id"]=="r7-pwd" && (v["type"]=="function_call_output" || v["type"]=="custom_tool_call_output")).expect("actual native pwd tool output")["output"].to_string();
                        assert!(output.contains(original.to_str().unwrap()),"{output}"); assert!(!output.contains(wrong.to_str().unwrap()),"{output}");
                        native_reply(2,"R7_RESUME_DONE")
                    }
                    _ => panic!("unexpected automatic conversation request"),
                }
            }
        });
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        std::fs::write(fixture.home.join("config.toml"),format!("model=\"gpt-6-astra\"\nmodel_provider=\"custom\"\ncheck_for_update_on_startup=false\napproval_policy=\"never\"\nsandbox_mode=\"workspace-write\"\n[features]\ncode_mode=false\ndaemon_auto_start=false\n[model_providers.custom]\nname=\"Synthetic R7\"\nbase_url=\"http://{address}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nexperimental_bearer_token=\"synthetic-r7-native\"\n")).unwrap();
        let mut seed =
            NativeProbe::start(&fixture.home, &fixture.cwd, &["--no-daemon".into()]).unwrap();
        seed.ready().await.unwrap();
        seed.submit("R7_SEED").await.unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            while !seed.screen().contains("R7_SEED_REPLY") {
                seed.pump().await.unwrap();
            }
        })
        .await
        .unwrap();
        seed.quit().await.unwrap();
        drop(seed);
        let id = thread.lock().unwrap().unwrap();
        let sessions = fixture.home.join("sessions");
        std::fs::write(sessions.join("synthetic-other.jsonl"),format!("{}\n",json!({"type":"session_meta","payload":{"id":Uuid::new_v4(),"source":"cli","cwd":other}}))).unwrap();
        let cli = PathBuf::from(
            std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into()),
        );
        let app = fixture.app(Overrides {
            codex_bin: Some(cli),
            ..Default::default()
        });
        let cookie = pair(&app).await;
        let entry = entry_for(&app, &cookie, id).await;
        println!(
            "R7_NATIVE_CLASSIFICATION {}",
            json!({"kind":entry["conversationKind"],"basis":entry["kindBasis"]})
        );
        assert!(
            matches!(
                entry["conversationKind"].as_str(),
                Some("ordinary" | "unknown")
            ),
            "native metadata must not be invented"
        );
        let all =
            json_body(get(&app, &cookie, "/workbench/v1/library/entries?kind=native").await).await;
        let project = all["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["recordedCwd"] == other.to_str().unwrap())
            .expect("existing server project")["projectId"]
            .clone();
        let snapshot = |root: &Path| -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
            fn collect(root: &Path, map: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
                for e in std::fs::read_dir(root).unwrap() {
                    let p = e.unwrap().path();
                    if p.is_dir() {
                        collect(&p, map)
                    } else if p.is_file() {
                        map.insert(p.clone(), std::fs::read(p).unwrap());
                    }
                }
            }
            let mut map = std::collections::BTreeMap::new();
            collect(root, &mut map);
            map
        };
        let before = snapshot(&fixture.home);
        for (path, bytes) in &before {
            if path.extension().is_some_and(|v| v == "jsonl")
                && let Some(meta) = String::from_utf8_lossy(bytes)
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .find(|v| v["type"] == "session_meta" && v["payload"]["id"] == id.to_string())
            {
                println!(
                    "R7_NATIVE_META_SOURCE {}",
                    json!({"source":meta["payload"]["source"]})
                );
            }
        }
        let path = format!(
            "/workbench/v1/library/entries/{}/organization",
            entry["entryId"].as_str().unwrap()
        );
        let read = get(&app, &cookie, &path).await;
        let rev = read.headers()["etag"].to_str().unwrap().to_owned();
        let saved = organize(&app, &cookie, &entry, "project", Some(&project), &rev).await;
        let status = saved.status();
        let saved = json_body(saved).await;
        assert_eq!(status, 200, "{saved}");
        assert_eq!(
            snapshot(&fixture.home),
            before,
            "idle organization must not modify native rollout/db/WAL/SHM/config"
        );
        let target=launch_post(&app,&cookie,"/workbench/v1/launch-targets",json!({"projectId":project,"resumeEntryId":entry["entryId"],"sourceRevision":entry["sourceRevision"]})).await;
        let status = target.status();
        let target = json_body(target).await;
        assert_eq!(status, 200, "{target}");
        assert_eq!(
            target["canonicalPath"],
            fixture.cwd.canonicalize().unwrap().to_str().unwrap()
        );
        let body = start_body(&app, &target);
        assert_eq!(
            launch_post(&app, &cookie, "/workbench/v1/runs", body.clone())
                .await
                .status(),
            202
        );
        let ready = operation_done(&app, &cookie, &body["operationId"]).await;
        assert_eq!(ready["state"], "ready", "{ready}");
        let run = &ready["run"];
        assert_eq!(run["resume"], id.to_string());
        let handle = terminal(&app, serde_json::from_value(run["runId"].clone()).unwrap()).await;
        let mut page = handle.attach().await.unwrap();
        let grant = handle.claim(page.connection_id).await.unwrap();
        let mut vt = vt100::Parser::new(page.snapshot.rows, page.snapshot.cols, 0);
        vt.process(&page.snapshot.screen);
        vt.process(&page.snapshot.replay);
        let mut sequence = 0;
        let mut trusted = false;
        let mut themed = false;
        let mut ready_at = None;
        let mut native_lock_retry_at = None;
        let mut native_lock_retries = 0;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let text = vt.screen().contents();
                if text.contains("This conversation is open in another app")
                    && native_lock_retries < 3
                    && native_lock_retry_at
                        .is_none_or(|at: std::time::Instant| at.elapsed() >= Duration::from_secs(1))
                {
                    native_lock_retry_at = Some(std::time::Instant::now());
                    native_lock_retries += 1;
                    sequence += 1;
                    handle
                        .input(
                            page.connection_id,
                            grant.generation,
                            sequence,
                            b"r".to_vec(),
                        )
                        .await
                        .unwrap();
                }
                if (!trusted
                    && (text.contains("Do you trust")
                        || text.contains("Do you want to work")
                        || text.contains("Trust this folder?")))
                    || (!themed
                        && (text.contains("Choose your style") || text.contains("Select a theme")))
                {
                    trusted = true;
                    themed = true;
                    sequence += 1;
                    handle
                        .input(
                            page.connection_id,
                            grant.generation,
                            sequence,
                            b"\r".to_vec(),
                        )
                        .await
                        .unwrap();
                }
                let (row, col) = vt.screen().cursor_position();
                let prompt = !vt.screen().hide_cursor()
                    && col > 0
                    && vt
                        .screen()
                        .rows(0, vt.screen().size().1)
                        .nth(usize::from(row))
                        .is_some_and(|v| v.trim_start().starts_with('›'));
                if text.contains("OpenAI Codex") && prompt {
                    if ready_at
                        .get_or_insert_with(std::time::Instant::now)
                        .elapsed()
                        >= Duration::from_millis(300)
                    {
                        break;
                    }
                } else {
                    ready_at = None;
                }
                if let Ok(Some(TerminalEvent::Output(output))) =
                    tokio::time::timeout(Duration::from_millis(50), page.events.recv()).await
                {
                    vt.process(&output.data);
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("native resume readiness: {}", vt.screen().contents()));
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(seen.load(Ordering::SeqCst), 1, "resume must not replay");
        sequence += 1;
        handle
            .input(
                page.connection_id,
                grant.generation,
                sequence,
                b"\x1b[200~R7_NEXT\x1b[201~".to_vec(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !vt.screen().contents().contains("R7_NEXT") {
                if let Some(TerminalEvent::Output(output)) = page.events.recv().await {
                    vt.process(&output.data);
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "native resume draft not displayed: {}",
                vt.screen().contents()
            )
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        sequence += 1;
        handle
            .input(
                page.connection_id,
                grant.generation,
                sequence,
                b"\r".to_vec(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            while !vt.screen().contents().contains("R7_RESUME_DONE") {
                if let Some(TerminalEvent::Output(output)) = page.events.recv().await {
                    vt.process(&output.data);
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(seen.load(Ordering::SeqCst), 3);
        println!(
            "R7_NATIVE_RESUME_PROOF {}",
            json!({"cliVersion":run["cliVersion"],"threadId":id,"classificationChanged":true,"idleNativeTreeUnchanged":true,"actualToolCwdOriginal":true,"noReplay":true})
        );
        handle.stop().await.unwrap();
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_applications_share_organization_cas_and_immediate_read_visibility() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4();
        std::fs::create_dir_all(fixture.home.join("sessions")).unwrap();
        let source = fixture.home.join("sessions/shared.jsonl");
        let raw = format!(
            "{}\n",
            json!({"type":"session_meta","payload":{"id":thread,"source":"cli","cwd":fixture.cwd}})
        );
        std::fs::write(&source, &raw).unwrap();
        let data = fixture.dir.path().join("shared-data");
        let make_app = |name: &str| {
            let config = fixture.dir.path().join(name);
            let prepared = Prepared::load_application(
                &fixture.home,
                &fixture.cwd,
                &fixture.paths,
                Some(&config),
                Overrides {
                    data_dir: Some(data.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
            let Acquisition::Owner(lock) =
                InstanceLock::acquire(&fixture.paths, prepared.config_dir(), &prepared.data_dir)
                    .unwrap()
            else {
                panic!("different config dirs must own different applications")
            };
            Application::start(
                prepared,
                fixture.home.clone(),
                fixture.paths.clone(),
                lock,
                None,
            )
            .unwrap()
        };
        let a = make_app("config-a");
        let ca = pair(&a).await;
        let ea = entry_for(&a, &ca, thread).await;
        let b = make_app("config-b");
        let cb = pair(&b).await;
        let eb = entry_for(&b, &cb, thread).await;
        assert_ne!(a.instance_id, b.instance_id);
        assert_eq!(ea["entryId"], eb["entryId"]);
        let path = format!(
            "/workbench/v1/library/entries/{}/organization",
            ea["entryId"].as_str().unwrap()
        );
        let ra = get(&a, &ca, &path).await;
        let rb = get(&b, &cb, &path).await;
        let rev = ra.headers()["etag"].to_str().unwrap().to_owned();
        assert_eq!(rb.headers()["etag"].to_str().unwrap(), rev);
        let (wa, wb) = tokio::join!(
            organize(&a, &ca, &ea, "unassigned", None, &rev),
            organize(&b, &cb, &eb, "automatic", None, &rev)
        );
        let statuses = [wa.status().as_u16(), wb.status().as_u16()];
        assert!(
            statuses == [200, 409] || statuses == [409, 200],
            "{statuses:?}"
        );
        let winner = if wa.status() == 200 { wa } else { wb };
        let won_rev = winner.headers()["etag"].to_str().unwrap().to_owned();
        let won = json_body(winner).await;
        for (app, cookie) in [(&a, ca.as_str()), (&b, cb.as_str())] {
            let read = get(app, cookie, &path).await;
            assert_eq!(read.status(), 200);
            assert_eq!(read.headers()["etag"].to_str().unwrap(), won_rev);
            assert_eq!(json_body(read).await, won);
        }
        assert_eq!(std::fs::read_to_string(source).unwrap(), raw);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn organization_sqlite_busy_does_not_block_two_active_ptys_or_model_proxies() {
        let fixture = Fixture::new();
        let cli = fixture.cli();
        let seen = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let counter = seen.clone();
        let router = axum::Router::new().fallback(move |request: axum::extract::Request| {
            let counter = counter.clone();
            async move {
                assert_eq!(
                    request.headers().get("authorization").unwrap(),
                    "Bearer synthetic-token"
                );
                let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
                    .await
                    .unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                counter.fetch_add(1, Ordering::SeqCst);
                let marker = body["marker"].as_str().unwrap();
                axum::response::Response::builder()
                    .header("Content-Type", "text/event-stream")
                    .body(axum::body::Body::from(format!(
                        "data: {}\n\n",
                        json!({"type":"response.completed","response":{"id":marker}})
                    )))
                    .unwrap()
            }
        });
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let config = std::fs::read_to_string(fixture.home.join("config.toml"))
            .unwrap()
            .replace("http://127.0.0.1:1/v1", &format!("http://{address}/v1"));
        std::fs::write(fixture.home.join("config.toml"), config).unwrap();
        let script = std::fs::read_to_string(&cli).unwrap().replace("#!/bin/sh\n","#!/bin/sh\nexport HOME=\"$CODEX_HOME\" USERPROFILE=\"$CODEX_HOME\"\n")
            .replace("exec /bin/cat","printf '%s\\n' \"$@\" > cli-args.txt\nstty -echo\nwhile IFS= read -r line; do printf '%s\\n' \"$line\" >> inputs.txt; done");
        std::fs::write(&cli, script).unwrap();
        let thread = Uuid::new_v4();
        std::fs::create_dir_all(fixture.home.join("sessions")).unwrap();
        let source = fixture.home.join("sessions/synthetic-r7.jsonl");
        let raw = format!(
            "{}\n",
            json!({"type":"session_meta","payload":{"id":thread,"source":"cli","cwd":fixture.cwd}})
        );
        std::fs::write(&source, &raw).unwrap();
        let app = fixture.app(Overrides {
            codex_bin: Some(cli),
            ..Default::default()
        });
        let cookie = pair(&app).await;
        let entry = entry_for(&app, &cookie, thread).await;
        let organization_path = format!(
            "/workbench/v1/library/entries/{}/organization",
            entry["entryId"].as_str().unwrap()
        );
        let read = get(&app, &cookie, &organization_path).await;
        let etag = read.headers()["etag"].to_str().unwrap().to_owned();
        let initialized = organize(&app, &cookie, &entry, "unassigned", None, &etag).await;
        assert_eq!(initialized.status(), 200);
        let etag = initialized.headers()["etag"].to_str().unwrap().to_owned();
        let mut owners = Vec::new();
        for index in 0..2 {
            let cwd = fixture.dir.path().join(format!("r7-active-{index}"));
            std::fs::create_dir(&cwd).unwrap();
            let summary = run(&app, &cookie, &cwd).await;
            let id: Uuid = serde_json::from_value(summary["runId"].clone()).unwrap();
            let handle = terminal(&app, id).await;
            let page = handle.attach().await.unwrap();
            let grant = handle.claim(page.connection_id).await.unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                while !cwd.join("cli-args.txt").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let args = std::fs::read_to_string(cwd.join("cli-args.txt")).unwrap();
            let base = args
                .lines()
                .find_map(|s| s.strip_prefix("model_providers.custom.base_url="))
                .unwrap();
            let parsed: toml::Value = toml::from_str(&format!("base={base}")).unwrap();
            owners.push((
                summary,
                handle,
                page,
                grant.generation,
                cwd,
                parsed["base"].as_str().unwrap().to_owned(),
            ));
        }
        // Before-proof: both PTYs and both real per-Run proxies are exercised
        // before introducing SQLite contention, not inferred from older tests.
        for (index, (_, handle, page, generation, cwd, base)) in owners.iter().enumerate() {
            handle
                .input(
                    page.connection_id,
                    *generation,
                    1,
                    format!("before-{index}\n").into_bytes(),
                )
                .await
                .unwrap();
            consumed(&cwd.join("inputs.txt"), &format!("before-{index}\n")).await;
            let reply = client()
                .post(format!("{base}/responses"))
                .bearer_auth("synthetic-token")
                .body(json!({"marker":format!("before-{index}")}).to_string())
                .send()
                .await
                .unwrap();
            assert_eq!(reply.status(), 200);
            assert!(
                reply
                    .text()
                    .await
                    .unwrap()
                    .contains(&format!("before-{index}"))
            );
        }
        let busy = rusqlite::Connection::open(
            fixture
                .paths
                .history_dir()
                .join("organization/state.sqlite"),
        )
        .unwrap();
        busy.execute_batch("BEGIN IMMEDIATE").unwrap();
        let write = organize(&app, &cookie, &entry, "automatic", None, &etag);
        let traffic = async {
            for (index, (_, handle, page, generation, cwd, base)) in owners.iter().enumerate() {
                handle
                    .input(
                        page.connection_id,
                        *generation,
                        2,
                        format!("during-{index}\n").into_bytes(),
                    )
                    .await
                    .unwrap();
                consumed(
                    &cwd.join("inputs.txt"),
                    &format!("before-{index}\nduring-{index}\n"),
                )
                .await;
                let reply = tokio::time::timeout(
                    Duration::from_secs(2),
                    client()
                        .post(format!("{base}/responses"))
                        .bearer_auth("synthetic-token")
                        .body(json!({"marker":format!("during-{index}")}).to_string())
                        .send(),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(reply.status(), 200);
                assert!(
                    reply
                        .text()
                        .await
                        .unwrap()
                        .contains(&format!("during-{index}"))
                );
            }
        };
        let (failed, ()) = tokio::join!(write, traffic);
        assert_eq!(failed.status(), 503);
        assert_eq!(
            json_body(failed).await["error"]["code"],
            "organization_unavailable"
        );
        assert_eq!(seen.load(Ordering::SeqCst), 4);
        let read = get(&app, &cookie, &organization_path).await;
        assert!(matches!(read.status().as_u16(), 200 | 503));
        busy.execute_batch("ROLLBACK").unwrap();
        let read = get(&app, &cookie, &organization_path).await;
        assert_eq!(read.status(), 200);
        assert_eq!(read.headers()["etag"].to_str().unwrap(), etag);
        assert_eq!(std::fs::read_to_string(source).unwrap(), raw);
        for (summary, _, _, _, cwd, _) in &owners {
            let target = target_for(&app, &cookie, cwd).await;
            let body = start_body(&app, &target);
            launch_post(&app, &cookie, "/workbench/v1/runs", body.clone()).await;
            assert_eq!(
                operation_done(&app, &cookie, &body["operationId"]).await["state"],
                "existing"
            );
            assert_eq!(
                json_body(
                    get(
                        &app,
                        &cookie,
                        &format!("/workbench/v1/runs/{}", summary["runId"].as_str().unwrap())
                    )
                    .await
                )
                .await["cliPid"],
                summary["cliPid"]
            );
        }
        assert_eq!(
            get(&app, "workbench_remote=fake", &organization_path)
                .await
                .status(),
            401
        );
        server.abort();
    }
}
