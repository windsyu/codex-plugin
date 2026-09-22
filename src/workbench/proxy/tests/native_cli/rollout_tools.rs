//! Installed-CLI proof for results which arrive after the last model request.
use super::launcher::{LauncherChild, launcher_command, wait_entry};
use super::*;
use crate::workbench::rollout::RolloutReader;
use crate::workbench::test_native::NativeProbe;
use std::io::BufRead;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

pub(super) fn native_items(home: &Path) -> Vec<Value> {
    native_events(home)
        .into_iter()
        .filter(|event| event["type"] == "item_completed")
        .collect()
}
pub(super) fn native_events(home: &Path) -> Vec<Value> {
    walkdir::WalkDir::new(home.join("sessions"))
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "jsonl")
        })
        .flat_map(|entry| {
            std::io::BufReader::new(std::fs::File::open(entry.path()).unwrap())
                .lines()
                .map_while(Result::ok)
                .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
                .filter(|line| line["type"] == "event_msg")
                .map(|line| line["payload"].clone())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed CLI; synthetic long command, temporary home, no real model"]
async fn native_command_finishes_after_last_model_request_and_updates_the_original_card() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let domain = Arc::new(Mutex::new(Value::Null));
    let observed_domain = domain.clone();
    let upstream = fixture(move |request| {
        let (seen, domain) = (seen.clone(), observed_domain.clone());
        async move {
            let body: Value = serde_json::from_slice(&axum::body::to_bytes(request.into_body(), 2 * 1024 * 1024).await.unwrap()).unwrap();
            if let Some(title) = title_response(&body) { return title; }
            assert!(tools_browser::advertises(&body, "exec_command"), "probe must use an advertised native tool");
            let index = seen.fetch_add(1, Ordering::SeqCst) + 1;
            if index == 1 {
                *domain.lock().unwrap() = serde_json::from_str(body["client_metadata"]["x-codex-turn-metadata"].as_str().unwrap()).unwrap();
                let call = json!({"type":"function_call", "id":"tool_native_late", "call_id":"call_native_late", "name":"exec_command", "arguments":json!({"cmd":"sleep 3; printf 'R2_NATIVE_LATE_RESULT\\n'; exit 7", "login":false, "yield_time_ms":1000, "max_output_tokens":200}).to_string()});
                let bytes = [event(json!({"type":"response.created", "response":{"id":"resp_native_late"}})), event(json!({"type":"response.output_item.done", "output_index":0, "item":call})), event(json!({"type":"response.completed", "response":{"id":"resp_native_late"}}))].concat();
                Response::builder().header(header::CONTENT_TYPE, "text/event-stream").body(Body::from(bytes)).unwrap()
            } else {
                assert_eq!(index, 2, "no polling or further model request is expected");
                assert!(body["input"].to_string().contains("Process running with session ID"));
                interaction::final_message(2, "R2_MODEL_FINISHED_WHILE_COMMAND_RUNNING")
            }
        }
    }).await;
    let (capture, receiver) = capture::channel(8 * 1024 * 1024, 256);
    let proxy = proxy_for(upstream.address, capture).await;
    let hub = LiveHub::new(LiveLimits::default());
    let policy = RedactionPolicy::new(Vec::new()).unwrap();
    let _observer = Observer::start(
        receiver,
        hub.clone(),
        policy.clone(),
        DecoderLimits::default(),
    )
    .unwrap();
    std::fs::write(
        home.join("config.toml"),
        format!(
            r#"model="gpt-5.5"
model_provider="custom"
check_for_update_on_startup=false
[model_providers.custom]
name="Synthetic native tool evidence"
base_url="{}"
wire_api="responses"
requires_openai_auth=false
[features]
code_mode=false
code_mode_only=false
"#,
            proxy.child_base_url()
        ),
    )
    .unwrap();
    let _reader = RolloutReader::start(&home, hub.clone(), policy).unwrap();
    let mut cli = NativeProbe::start(
        &home,
        &workspace,
        &["-c".into(), "tui.animations=false".into()],
    )
    .unwrap();
    cli.ready().await.unwrap();
    cli.submit("R2_NATIVE_LATE：运行给定的合成长命令。")
        .await
        .unwrap();
    let event = timeout(Duration::from_secs(15), async {
        loop {
            cli.pump().await.unwrap();
            if let Some(item) = native_items(&home).into_iter().find(|event| {
                event["item"]["type"] == "CommandExecution"
                    && event["item"]["id"] == "call_native_late"
            }) {
                break item;
            }
        }
    })
    .await
    .expect("native final command evidence deadline");
    let domain = domain.lock().unwrap().clone();
    assert_eq!(event["thread_id"], domain["thread_id"]);
    assert_eq!(event["turn_id"], domain["turn_id"]);
    assert_eq!(event["item"]["exit_code"], 7);
    assert_eq!(event["item"]["status"], "failed");
    assert_eq!(
        event["item"]["aggregated_output"],
        "R2_NATIVE_LATE_RESULT\n"
    );
    assert!(
        event["item"]["process_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    println!(
        "{}",
        json!({"check":"native-command-rollout-shape", "itemType":event["item"]["type"], "status":event["item"]["status"], "source":event["item"]["source"], "duration":event["item"]["duration"], "callIdMatched":true, "explicitThreadAndTurn":true, "requests":count.load(Ordering::SeqCst)})
    );
    let updated = timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = hub.snapshot();
            if snapshot.tools().first().is_some_and(|tool| {
                tool.execution == crate::workbench::live::ExecutionState::Failed
            }) {
                break;
            }
            cli.pump().await.unwrap();
        }
    })
    .await;
    let check = hub.snapshot();
    println!(
        "{}",
        json!({"check":"native-command-reconciliation", "nativeCommands":check.native_commands.len(), "userMessages":check.user_messages().len(), "diagnostics":check.user_capture.diagnostics, "tools":check.tools().iter().map(|tool| json!({"category":tool.category, "callId":tool.identity.call_id, "execution":tool.execution, "identityConflict":tool.identity_conflict, "resultConflict":tool.result_conflict})).collect::<Vec<_>>()})
    );
    updated.expect("rollout final result must update the card without another model request");
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.tools().len(), 1);
    assert_eq!(
        snapshot.tools()[0].result.as_ref().unwrap().output,
        "R2_NATIVE_LATE_RESULT\n"
    );
    assert_eq!(
        snapshot.tools()[0].result.as_ref().unwrap().exit_code,
        Some(7)
    );
    cli.quit().await.unwrap();
}

fn tool_response(index: usize, call: Value) -> Response {
    let id = format!("resp_native_browser_{index}");
    let bytes = [
        event(json!({"type":"response.created","response":{"id":id}})),
        event(json!({"type":"response.output_item.done","output_index":0,"item":call})),
        event(json!({"type":"response.completed","response":{"id":id}})),
    ]
    .concat();
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(bytes))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view and installed CLI/Chrome; synthetic commands only"]
async fn browser_keeps_the_original_command_card_through_late_exit_and_native_polling() {
    for (polled, long_output) in [(false, false), (true, false), (false, true)] {
        browser_scenario(polled, long_output).await;
    }
}

async fn browser_scenario(polled: bool, long_output: bool) {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("native-command-results");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    if long_output {
        std::fs::write(workspace.join("output.txt"), format!(
            "R2_BROWSER_NATIVE_FINAL\n<svg onload=globalThis.r2Unsafe=1>literal</svg>\nBearer r2-output-credential\n{}\n{{\"t\\u006fken\":\"synthetic-field-value\"}}\n",
            format!("{}\n", "长输出 ".repeat(20)).repeat(2000)
        )).unwrap();
    }
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let upstream = fixture(move |request| {
        let seen = seen.clone();
        async move {
            let body:Value=serde_json::from_slice(&axum::body::to_bytes(request.into_body(),2*1024*1024).await.unwrap()).unwrap();
            if let Some(title)=title_response(&body) { return title; }
            assert!(tools_browser::advertises(&body,"exec_command"));
            let index=seen.fetch_add(1,Ordering::SeqCst)+1;
            if index==1 {
                let command = if long_output { "while ! test -f finish; do sleep 0.05; done; cat output.txt; exit 7" } else { "while ! test -f finish; do sleep 0.05; done; printf 'R2_BROWSER_NATIVE_FINAL\\n'; exit 7" };
                let args=json!({"cmd":command,"login":false,"yield_time_ms":1000,"max_output_tokens":200});
                return tool_response(index,json!({"type":"function_call","id":"tool_browser_native","call_id":"call_browser_native","name":"exec_command","arguments":args.to_string()}));
            }
            if polled && index==2 {
                assert!(tools_browser::advertises(&body,"write_stdin"));
                let result=body["input"].as_array().unwrap().iter().find(|item| item["type"]=="function_call_output" && item["call_id"]=="call_browser_native").unwrap();
                let (facts,_)=crate::workbench::decode::tool_context::command_facts(result["output"].as_str().unwrap()).unwrap();
                let process=facts.process_id.unwrap().parse::<u32>().unwrap();
                return tool_response(index,json!({"type":"function_call","id":"tool_browser_poll","call_id":"call_browser_poll","name":"write_stdin","arguments":json!({"session_id":process,"chars":"","yield_time_ms":1000,"max_output_tokens":200}).to_string()}));
            }
            assert_eq!(index,if polled {3}else{2});
            if polled {
                let output=body["input"].as_array().unwrap().iter().find(|item| item["type"]=="function_call_output" && item["call_id"]=="call_browser_poll").unwrap();
                assert!(output["output"].as_str().unwrap().contains("Process exited with code 7"),"native polling must return the final exit");
            }
            interaction::final_message(index,"R2_BROWSER_MODEL_FINISHED")
        }
    }).await;
    let config = format!(
        r#"model="gpt-5.5"
model_provider="custom"
check_for_update_on_startup=false
approval_policy="never"
sandbox_mode="workspace-write"
[model_providers.custom]
name="Synthetic native command results"
base_url="http://{}/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-native-results"
[features]
code_mode=false
code_mode_only=false
"#,
        upstream.address
    );
    std::fs::write(home.join("config.toml"), config).unwrap();
    let entry_file = directory.path().join("entry.json");
    let mut launcher = LauncherChild(
        launcher_command(&home, &workspace, &entry_file)
            .spawn()
            .unwrap(),
    );
    let entry = wait_entry(&mut launcher, &entry_file).await;
    let mut browser = tokio::process::Command::new("node");
    browser
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r2-native-result-probe.cjs"))
        .env("WORKBENCH_PROBE_URL", &entry.url)
        .env(
            "WORKBENCH_PROBE_POLLED",
            if polled { "true" } else { "false" },
        )
        .env(
            "WORKBENCH_PROBE_LONG_OUTPUT",
            if long_output { "true" } else { "false" },
        )
        .env("DEBUG", "")
        .env("PWDEBUG", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("WORKBENCH_TEST_SCREENSHOT") {
        browser.env("WORKBENCH_PROBE_SCREENSHOT", path);
    }
    let mut browser = crate::workbench::probe_process::ProbeProcess::spawn(&mut browser).unwrap();
    let _liveness = browser.stdin.take();
    let mut lines = BufReader::new(browser.stdout.take().unwrap()).lines();
    let mut saw_running = false;
    timeout(Duration::from_secs(60), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let event: Value = serde_json::from_str(&line).unwrap();
            println!("{event}");
            if event["stage"] == "running" {
                saw_running = true;
                if polled {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                std::fs::write(workspace.join("finish"), "synthetic test gate").unwrap();
            }
        }
        assert!(
            browser.wait().await.unwrap().success(),
            "native result browser assertions failed"
        );
    })
    .await
    .unwrap();
    assert!(saw_running);
    assert_eq!(count.load(Ordering::SeqCst), if polled { 3 } else { 2 });
    println!(
        "{}",
        json!({"check":"native-result-browser","polled":polled,"longOutput":long_output,"mainRequests":count.load(Ordering::SeqCst),"noInputReplay":true})
    );
}
