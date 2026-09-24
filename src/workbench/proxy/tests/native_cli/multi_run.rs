//! R6-F real installed CLI and system Chrome, isolated synthetic model and homes.
use super::launcher::LauncherChild;
use super::*;
use std::path::Path;
use std::process::Stdio;

fn reply(label: &'static str, index: usize) -> Response {
    let stream = async_stream::stream! {
        let response = format!("resp_multi_{label}_{index}");
        let item = format!("msg_multi_{label}_{index}");
        yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":response,"model":"gpt-6-astra"}})));
        yield Ok(event(json!({"type":"response.output_item.added","item":{"id":item,"type":"message","role":"assistant","content":[]}})));
        let first = format!("R6_F_{label}_中文流式.");
        yield Ok(event(json!({"type":"response.output_text.delta","item_id":item,"content_index":0,"delta":first})));
        tokio::time::sleep(Duration::from_secs(3)).await;
        let text = format!("{first} R6_F_{label}_DONE_{index}");
        yield Ok(event(json!({"type":"response.output_text.delta","item_id":item,"content_index":0,"delta":format!(" R6_F_{label}_DONE_{index}")})));
        yield Ok(event(json!({"type":"response.output_item.done","item":{"id":item,"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}})));
        let tokens = if label == "A" { 100 } else { 200 };
        yield Ok(event(json!({"type":"response.completed","response":{"id":response,"usage":{"input_tokens":tokens,"output_tokens":10,"total_tokens":tokens+10}}})));
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires built codex-view, installed official CLI and system Chrome; fully isolated homes"]
async fn product_parallel_projects_keep_native_streams_files_usage_and_stop_isolated() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("native");
    let workspace = directory.path().join("项目 A with spaces");
    let other = directory.path().join("项目 B with spaces");
    for path in [&home, &workspace, &other] {
        std::fs::create_dir(path).unwrap();
    }
    for (path, label) in [(&workspace, "A"), (&other, "B")] {
        std::fs::write(path.join("R6-check.txt"), format!("R6_F_{label}_FILE\n")).unwrap();
    }
    let count = Arc::new(AtomicUsize::new(0));
    let identities = Arc::new(Mutex::new(
        std::collections::HashMap::<String, String>::new(),
    ));
    let (seen, threads) = (count.clone(), identities.clone());
    let upstream = fixture(move |request| {
        let (seen, threads) = (seen.clone(), threads.clone());
        async move {
            assert_eq!(
                request.headers().get(header::AUTHORIZATION).unwrap(),
                "Bearer synthetic-r6-multi"
            );
            let body: Value = serde_json::from_slice(
                &axum::body::to_bytes(request.into_body(), 2 * 1024 * 1024)
                    .await
                    .unwrap(),
            )
            .unwrap();
            if let Some(title) = title_response(&body) {
                return title;
            }
            let input = body["input"].to_string();
            let label = if input.contains("R6_F_A_INPUT") {
                "A"
            } else {
                "B"
            };
            assert!(input.contains(&format!("R6_F_{label}_INPUT")));
            assert!(!input.contains(if label == "A" {
                "R6_F_B_INPUT"
            } else {
                "R6_F_A_INPUT"
            }));
            let metadata: Value = serde_json::from_str(
                body["client_metadata"]["x-codex-turn-metadata"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            let id = metadata["thread_id"].as_str().unwrap().to_owned();
            let mut guard = threads.lock().unwrap();
            let previous = guard.insert(label.to_owned(), id.clone());
            let first = previous.is_none();
            if let Some(previous) = previous {
                assert_eq!(previous, id);
            }
            if guard.len() == 2 {
                assert_ne!(guard["A"], guard["B"]);
            }
            drop(guard);
            let index = seen.fetch_add(1, Ordering::SeqCst);
            let call_id = format!("call_multi_{label}");
            if first {
                let code = tools_browser::advertises(&body, "exec");
                assert!(code || tools_browser::advertises(&body, "exec_command"));
                let arguments = json!({"cmd":"pwd && cat R6-check.txt", "login":false, "max_output_tokens":300,"yield_time_ms":1000}).to_string();
                let call = if code { json!({"type":"custom_tool_call","id":format!("ctc_multi_{label}"),"call_id":call_id,"name":"exec","input":format!("text((await tools.exec_command({arguments})).output);")}) } else { json!({"type":"function_call","id":format!("fc_multi_{label}"),"call_id":call_id,"name":"exec_command","arguments":arguments}) };
                let bytes = [event(json!({"type":"response.created","response":{"id":format!("resp_tool_{label}")}})),event(json!({"type":"response.output_item.done","output_index":0,"item":call})),event(json!({"type":"response.completed","response":{"id":format!("resp_tool_{label}")}}))].concat();
                return Response::builder().header(header::CONTENT_TYPE,"text/event-stream").body(Body::from(bytes)).unwrap();
            }
            let output = body["input"].as_array().unwrap().iter().find(|item| (item["type"] == "function_call_output" || item["type"] == "custom_tool_call_output") && item["call_id"] == call_id).expect("actual native tool result");
            let output = output["output"].to_string();
            assert!(output.contains(&format!("R6_F_{label}_FILE")), "native command must read its own project file");
            assert!(output.contains(&format!("项目 {label} with spaces")), "native command must execute in its own project directory");
            assert!(!output.contains(if label == "A" { "R6_F_B_FILE" } else { "R6_F_A_FILE" }));
            reply(label, index)
        }
    })
    .await;
    let config = format!(
        "model=\"gpt-6-astra\"\nmodel_provider=\"custom\"\ncheck_for_update_on_startup=false\napproval_policy=\"never\"\nsandbox_mode=\"workspace-write\"\n[features]\ncode_mode=false\n[model_providers.custom]\nname=\"Synthetic R6 multi\"\nbase_url=\"http://{}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nexperimental_bearer_token=\"synthetic-r6-multi\"\n",
        upstream.address
    );
    std::fs::write(home.join("config.toml"), config).unwrap();
    let entry_file = directory.path().join("entry.json");
    let binary = std::env::var_os("WORKBENCH_TEST_LAUNCHER")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/codex-view"));
    let mut command = tokio::process::Command::new(binary);
    command
        .current_dir(directory.path())
        .env("HOME", directory.path())
        .env("USERPROFILE", directory.path())
        .env("CODEX_HOME", &home)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .args(["--no-open", "--entry-file"])
        .arg(&entry_file)
        .arg("--codex-bin")
        .arg(std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = LauncherChild(command.spawn().unwrap());
    let entry = timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(entry) = crate::workbench::launch::read_entry(&entry_file) {
                break entry;
            }
            assert!(child.0.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let mut browser = tokio::process::Command::new("node");
    browser
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r6-multi-run-probe.cjs"))
        .env("WORKBENCH_PROBE_URL", &entry.url)
        .env(
            "WORKBENCH_PROBE_PROJECT_A",
            workspace.canonicalize().unwrap(),
        )
        .env("WORKBENCH_PROBE_PROJECT_B", other.canonicalize().unwrap())
        .env(
            "WORKBENCH_PROBE_RESULT",
            directory.path().join("probe-result.json"),
        )
        .env("HOME", directory.path())
        .env("USERPROFILE", directory.path())
        .env("CODEX_HOME", &home)
        .env("DEBUG", "")
        .env("PWDEBUG", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("WORKBENCH_TEST_SCREENSHOT") {
        browser.env("WORKBENCH_PROBE_SCREENSHOT", path);
    }
    let mut browser = crate::workbench::probe_process::ProbeProcess::spawn(&mut browser).unwrap();
    let _liveness = browser.stdin.take();
    assert!(
        timeout(Duration::from_secs(125), browser.wait())
            .await
            .unwrap()
            .unwrap()
            .success(),
        "R6 homepage native browser probe failed"
    );
    assert_eq!(count.load(Ordering::SeqCst), 5);
    assert_eq!(identities.lock().unwrap().len(), 2);
    assert_eq!(
        unsafe { libc::kill(child.0.id().unwrap() as i32, libc::SIGTERM) },
        0
    );
    assert!(
        timeout(Duration::from_secs(5), child.0.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!entry_file.exists());
    let result: Value =
        serde_json::from_slice(&std::fs::read(directory.path().join("probe-result.json")).unwrap())
            .unwrap();
    for pid in result["pids"].as_array().unwrap() {
        assert_eq!(
            unsafe { libc::kill(pid.as_i64().unwrap() as i32, 0) },
            -1,
            "Application must reap every owned CLI"
        );
    }
}
