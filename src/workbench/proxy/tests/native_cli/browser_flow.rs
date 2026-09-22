//! Real native picker, approval and question inside the production browser terminal.
use super::launcher::{LauncherChild, launcher_command, wait_entry};
use super::*;
use std::path::Path;
use std::process::Stdio;

fn tool_response(index: usize, name: &str, arguments: Value) -> Response {
    let response = format!("resp_native_flow_{index}");
    let item = if name == "exec" {
        json!({"type":"custom_tool_call","id":format!("ctc_{index}"),"call_id":format!("call_native_flow_{index}"),"name":"exec","input":format!("text(await tools.exec_command({arguments}));")})
    } else {
        json!({"type":"function_call","id":format!("fc_{index}"),"call_id":format!("call_native_flow_{index}"),"name":name,"arguments":arguments.to_string()})
    };
    let bytes = [
        event(json!({"type":"response.created","response":{"id":response}})),
        event(json!({"type":"response.output_item.done","output_index":0,"item":item})),
        event(json!({"type":"response.completed","response":{"id":response}})),
    ]
    .concat();
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(bytes))
        .unwrap()
}

fn tools_include(tools: &Value, name: &str) -> bool {
    tools.as_array().is_some_and(|tools| {
        tools.iter().any(|tool| {
            tool["name"] == name
                || (tool["type"] == "namespace"
                    && tool["tools"]
                        .as_array()
                        .is_some_and(|nested| nested.iter().any(|tool| tool["name"] == name)))
        })
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and Chrome; only synthetic loopback data"]
async fn product_browser_native_picker_approval_question_and_short_disconnect() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("native-flow-project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let verified = Arc::new(Mutex::new(Vec::new()));
    let (seen, checks) = (count.clone(), verified.clone());
    let upstream = fixture(move |request| {
        let (seen, checks) = (seen.clone(), checks.clone());
        async move {
            assert_eq!(request.headers().get(header::AUTHORIZATION).unwrap(), "Bearer synthetic-native-flow");
            let body:Value = serde_json::from_slice(&axum::body::to_bytes(request.into_body(),2*1024*1024).await.unwrap()).unwrap();
            if let Some(title) = title_response(&body) { return title; }
            assert_eq!(body["model"], "gpt-6-astra", "cancelled picker must retain the native model");
            let index = seen.fetch_add(1,Ordering::SeqCst)+1;
            // Responses Lite uses an explicit developer additional_tools input,
            // not the top-level Responses tools field (installed native behavior).
            let has_tool = |name| tools_include(&body["tools"],name) || body["input"].as_array().is_some_and(|items|items.iter().any(|item|item["type"]=="additional_tools" && item["role"]=="developer" && tools_include(&item["tools"],name)));
            match index {
                1 => {
                    assert!(body["input"].to_string().contains("R1_APPROVAL_REQUEST"));
                    assert!(has_tool("exec"));
                    checks.lock().unwrap().push("approval-request");
                    tool_response(1,"exec",json!({"cmd":"printf 'R1_APPROVED_TOOL_OK\\n'","login":false,"max_output_tokens":100,"yield_time_ms":1000,"sandbox_permissions":"require_escalated","justification":"Print the synthetic R1 approval marker only."}))
                }
                2 => {
                    let item = body["input"].as_array().unwrap().iter().find(|item| item["type"]=="custom_tool_call_output" && item["call_id"]=="call_native_flow_1").unwrap();
                    assert!(item["output"].to_string().contains("R1_APPROVED_TOOL_OK"));
                    checks.lock().unwrap().push("approval-result");
                    interaction::final_message(2,"R1_APPROVAL_DONE")
                }
                3 => {
                    assert!(body["input"].to_string().contains("R1_QUESTION_REQUEST"));
                    assert!(has_tool("request_user_input"));
                    checks.lock().unwrap().push("question-request");
                    tool_response(3,"request_user_input",json!({"questions":[{"id":"sort_by","header":"Sort","question":"请选择本次合成测试的排序方式。","options":[{"label":"Name (Recommended)","description":"按名称排序。"},{"label":"Price","description":"按价格排序。"}]}]}))
                }
                4 => {
                    let item = body["input"].as_array().unwrap().iter().find(|item| item["type"]=="function_call_output" && item["call_id"]=="call_native_flow_3").unwrap();
                    let answer:Value=serde_json::from_str(item["output"].as_str().unwrap()).unwrap();
                    assert_eq!(answer["answers"]["sort_by"]["answers"],json!(["Price"]));
                    checks.lock().unwrap().push("question-result");
                    interaction::final_message(4,"R1_QUESTION_DONE")
                }
                _ => panic!("unexpected extra task or replay in native browser probe"),
            }
        }
    }).await;
    let config = format!(
        r#"model="gpt-6-astra"
model_provider="custom"
check_for_update_on_startup=false
approval_policy="on-request"
approvals_reviewer="user"
sandbox_mode="read-only"
[model_providers.custom]
name="Synthetic native flow"
base_url="http://{}/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-native-flow"
[features]
code_mode=true
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
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let mut browser = tokio::process::Command::new("node");
    browser
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r1-native-flow-probe.cjs"))
        .env("WORKBENCH_PROBE_URL", &entry.url)
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
        timeout(Duration::from_secs(120), browser.wait())
            .await
            .unwrap()
            .unwrap()
            .success(),
        "native browser flow failed; screenshot contains synthetic diagnostic state"
    );
    assert_eq!(count.load(Ordering::SeqCst), 4);
    assert_eq!(
        *verified.lock().unwrap(),
        vec![
            "approval-request",
            "approval-result",
            "question-request",
            "question-result"
        ]
    );
    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
    let before: toml::Value = toml::from_str(&config).unwrap();
    for field in [
        "model",
        "model_provider",
        "model_providers",
        "approval_policy",
        "approvals_reviewer",
        "sandbox_mode",
        "features",
    ] {
        assert_eq!(after[field], before[field]);
    }
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
    println!(
        "{}",
        json!({"check":"native-browser-flow","mainRequests":4,"toolResultMatched":true,"questionAnswerMatched":true,"configurationPreserved":true})
    );
}
