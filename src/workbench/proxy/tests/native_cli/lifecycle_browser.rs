//! Native refusal and cancellation boundaries, without inventing execution facts.
use super::launcher::{LauncherChild, launcher_command, wait_entry};
use super::*;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

fn tool_response(index: usize, name: &str, arguments: Value) -> Response {
    let id = format!("lifecycle-response-{index}");
    let item = json!({"type":"function_call","id":format!("lifecycle-item-{index}"),"call_id":format!("lifecycle-call-{index}"),"name":name,"arguments":arguments.to_string()});
    let bytes = [
        event(json!({"type":"response.created","response":{"id":id}})),
        event(json!({"type":"response.output_item.done","output_index":0,"item":item})),
        event(json!({"type":"response.completed","response":{"id":id}})),
    ]
    .concat();
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(bytes))
        .unwrap()
}

struct Cancelled(Arc<AtomicBool>);
impl Drop for Cancelled {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and Chrome; isolated refusal/cancel fixtures only"]
async fn native_tool_refusals_and_cancelled_parameters_remain_distinct_from_execution_facts() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("R2-lifecycle-project");
    std::fs::create_dir_all(home.join("rules")).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    let rules = "prefix_rule(pattern=[\"touch\"], decision=\"forbidden\")\n";
    std::fs::write(home.join("rules/fixture.rules"), rules).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicBool::new(false));
    let domains = Arc::new(Mutex::new(Vec::<Value>::new()));
    let (seen, dropped, observed, project) = (
        count.clone(),
        cancelled.clone(),
        domains.clone(),
        workspace.clone(),
    );
    let upstream = fixture(move |request| {
        let (seen, dropped, observed, project) = (seen.clone(), dropped.clone(), observed.clone(), project.clone());
        async move {
            let body: Value = serde_json::from_slice(&axum::body::to_bytes(request.into_body(),2*1024*1024).await.unwrap()).unwrap();
            if let Some(title) = title_response(&body) { return title; }
            let index = seen.fetch_add(1,Ordering::SeqCst)+1;
            observed.lock().unwrap().push(serde_json::from_str(body["client_metadata"]["x-codex-turn-metadata"].as_str().unwrap()).unwrap());
            assert!(!project.join("must-not-exist.txt").exists());
            if (2..=4).contains(&index) {
                let id = format!("lifecycle-call-{}",index-1);
                let result = body["input"].as_array().unwrap().iter().find(|item| item["type"]=="function_call_output" && item["call_id"]==id).expect("native refusal output");
                let output = result["output"].as_str().expect("native text output");
                if index == 2 {
                    let lower = output.to_ascii_lowercase();
                    assert!(lower.contains("reject") || lower.contains("forbidden") || lower.contains("policy"),"expected native policy refusal");
                } else {
                    assert!(output.contains(if index == 3 {"future_fixture_tool"} else {"cmd"}),"expected native tool/argument error");
                }
            }
            match index {
                1 => {
                    assert!(tools_browser::advertises(&body,"exec_command"));
                    tool_response(index,"exec_command",json!({"cmd":"touch must-not-exist.txt","login":false,"yield_time_ms":1000,"max_output_tokens":100}))
                }
                2 => tool_response(index,"future_fixture_tool",json!({"note":"R2_UNKNOWN_TOOL"})),
                3 => tool_response(index,"exec_command",json!({"workdir":project,"login":false})),
                4 => interaction::final_message(index,"R2_LIFECYCLE_RESULTS_DONE"),
                5 => {
                    assert!(body["input"].to_string().contains("R2_ABORT_PARAMETERS"));
                    assert!(tools_browser::advertises(&body,"apply_patch"));
                    let stream = async_stream::stream! {
                        let _cancelled = Cancelled(dropped);
                        yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":"lifecycle-incomplete"}})));
                        yield Ok(event(json!({"type":"response.output_text.delta","item_id":"lifecycle-partial-text","content_index":0,"delta":"R5_CANCELLED_TEXT_PARTIAL"})));
                        yield Ok(event(json!({"type":"response.output_item.added","output_index":0,"item":{"type":"custom_tool_call","id":"lifecycle-incomplete-item","call_id":"lifecycle-incomplete-call","name":"apply_patch","input":""}})));
                        yield Ok(event(json!({"type":"response.custom_tool_call_input.delta","item_id":"lifecycle-incomplete-item","delta":"*** Begin Patch\n*** Add File: cancelled.txt\n+R2_INCOMPLETE_PARAMETERS\n"})));
                        std::future::pending::<()>().await;
                    };
                    Response::builder().header(header::CONTENT_TYPE,"text/event-stream").body(Body::from_stream(stream)).unwrap()
                }
                6 => {
                    assert!(dropped.load(Ordering::SeqCst),"native cancellation must disconnect the pending model response");
                    assert!(body["input"].to_string().contains("R2_AFTER_ABORT"));
                    interaction::final_message(index,"R2_AFTER_ABORT_DONE")
                }
                _ => panic!("unexpected lifecycle request or replay"),
            }
        }
    }).await;
    let config = format!(
        r#"model="gpt-5.5"
model_provider="custom"
check_for_update_on_startup=false
approval_policy="never"
sandbox_mode="workspace-write"
[model_providers.custom]
name="Synthetic lifecycle"
base_url="http://{}/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-r2-lifecycle"
[features]
code_mode=false
code_mode_only=false
"#,
        upstream.address
    );
    std::fs::write(home.join("config.toml"), &config).unwrap();
    let entry_file = directory.path().join("entry.json");
    let mut child = LauncherChild(
        launcher_command(&home, &workspace, &entry_file)
            .spawn()
            .unwrap(),
    );
    let entry = wait_entry(&mut child, &entry_file).await;
    let mut command = tokio::process::Command::new("node");
    command
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r2-lifecycle-probe.cjs"))
        .env("WORKBENCH_PROBE_URL", &entry.url)
        .env("DEBUG", "")
        .env("PWDEBUG", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("WORKBENCH_TEST_SCREENSHOT") {
        command.env("WORKBENCH_PROBE_SCREENSHOT", path);
    }
    let mut browser = crate::workbench::probe_process::ProbeProcess::spawn(&mut command).unwrap();
    let _liveness = browser.stdin.take();
    let mut lines = BufReader::new(browser.stdout.take().unwrap()).lines();
    let mut complete = false;
    timeout(Duration::from_secs(55), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let report: Value = serde_json::from_str(&line).unwrap();
            println!("{report}");
            match report["stage"].as_str().unwrap() {
                "complete" => complete = true,
                _ => panic!("native lifecycle probe failed at safe stage"),
            }
        }
        assert!(browser.wait().await.unwrap().success());
    })
    .await
    .unwrap();
    assert!(complete);
    assert_eq!(count.load(Ordering::SeqCst), 6);
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
    let events = rollout_tools::native_events(&home);
    let tool_facts: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event["item"]["type"].as_str(),
                Some("CommandExecution" | "FileChange")
            )
        })
        .collect();
    assert!(
        tool_facts.is_empty(),
        "policy/validation refusal and incomplete parameters do not prove native execution"
    );
    let users: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "item_completed" && event["item"]["type"] == "UserMessage")
        .collect();
    assert_eq!(users.len(), 3);
    let domains = domains.lock().unwrap();
    assert!(
        domains[..4]
            .iter()
            .all(|meta| meta["turn_id"] == users[0]["turn_id"])
    );
    assert_eq!(domains[4]["turn_id"], users[1]["turn_id"]);
    assert_eq!(domains[5]["turn_id"], users[2]["turn_id"]);
    assert!(events.iter().any(|event| event["type"]=="turn_aborted" && event["turn_id"]==users[1]["turn_id"]));
    assert!(!workspace.join("must-not-exist.txt").exists());
    assert!(!workspace.join("cancelled.txt").exists());
    assert_eq!(
        std::fs::read_to_string(home.join("rules/fixture.rules")).unwrap(),
        rules
    );
    let before: toml::Value = toml::from_str(&config).unwrap();
    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
    for key in [
        "model",
        "model_provider",
        "model_providers",
        "approval_policy",
        "sandbox_mode",
        "features",
    ] {
        assert_eq!(after[key], before[key]);
    }
    println!(
        "{}",
        json!({"check":"native-lifecycle-evidence","nativeSubmissions":3,"modelRequests":6,"policyRefusalOutput":true,"unknownToolOutput":true,"invalidArgumentsOutput":true,"toolExecutionFacts":0,"turnAborted":true,"streamDisconnected":true,"workspacePreserved":true})
    );
}
