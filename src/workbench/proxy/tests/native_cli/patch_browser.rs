//! Actual native apply_patch over a synthetic model, never a user's workspace.
use super::launcher::{LauncherChild, launcher_command, wait_entry};
use super::*;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Notify;

const PROPOSAL: &str = "*** Begin Patch\n*** Add File: added.txt\n+R2_PATCH_ADDED\n+<img src=x onerror=\"window.patchInjected=1\">\n*** Update File: input.txt\n@@\n keep\n-before\n+after\n*** Update File: move-from.txt\n*** Move to: moved.txt\n@@\n-old\n+R2_PATCH_MOVED\n*** Delete File: delete.txt\n*** End Patch";
const FAILURE: &str =
    "*** Begin Patch\n*** Update File: missing.txt\n@@\n-before\n+after\n*** End Patch";
const EXECUTION_FAILURE: &str = "*** Begin Patch\n*** Add File: blocker/child.txt\n+R2 cannot create this directory\n*** End Patch";

fn reply(index: usize, gate: Arc<Notify>) -> Response {
    let raw = match index {
        1 => PROPOSAL,
        2 => FAILURE,
        _ => EXECUTION_FAILURE,
    };
    let item = json!({"id":format!("item_patch_{index}"),"call_id":format!("call_patch_{index}"),"type":"custom_tool_call","name":"apply_patch","input":raw});
    let mut initial = item.clone();
    initial["input"] = json!("");
    let stream = async_stream::stream! {
        let response = format!("resp_patch_{index}");
        yield Ok::<_,std::io::Error>(event(json!({"type":"response.created","response":{"id":response}})));
        yield Ok(event(json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","id":format!("msg_patch_{index}"),"content":[{"type":"output_text","text":match index {1=>"先提出新增、修改、移动和删除四个文件的 Diff。",2=>"文件修改已返回结果；接着观察一次缺失文件的校验错误。",_=>"缺失文件已返回错误；接着验证执行阶段的创建目录失败。"}}]}})));
        yield Ok(event(json!({"type":"response.output_item.added","output_index":1,"item":initial})));
        for (chunk_index, chunk) in raw.chars().collect::<Vec<_>>().chunks(15).enumerate() {
            yield Ok(event(json!({"type":"response.custom_tool_call_input.delta","item_id":format!("item_patch_{index}"),"delta":chunk.iter().collect::<String>()})));
            if index == 1 && chunk_index == 3 { gate.notified().await; }
            tokio::time::sleep(Duration::from_millis(8)).await;
        }
        yield Ok(event(json!({"type":"response.output_item.done","output_index":1,"item":item})));
        yield Ok(event(json!({"type":"response.completed","response":{"id":response}})));
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and Chrome; synthetic patch in temporary workspace only"]
async fn native_patch_diff_matches_actual_add_update_move_delete_and_failed_result() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("R2-patch-project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    for (path, content) in [
        ("blocker", "existing file\n"),
        ("input.txt", "keep\nbefore\n"),
        ("move-from.txt", "old\n"),
        ("delete.txt", "deleted body\n"),
    ] {
        std::fs::write(workspace.join(path), content).unwrap();
    }
    let gate = Arc::new(Notify::new());
    let server_gate = gate.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let project = workspace.clone();
    let upstream = fixture(move |request| {
        let (gate, seen, project) = (server_gate.clone(), seen.clone(), project.clone());
        async move {
            let body: Value = serde_json::from_slice(
                &axum::body::to_bytes(request.into_body(), 2 * 1024 * 1024)
                    .await
                    .unwrap(),
            )
            .unwrap();
            if let Some(title) = title_response(&body) {
                return title;
            }
            assert!(tools_browser::advertises(&body, "apply_patch"));
            let index = seen.fetch_add(1, Ordering::SeqCst) + 1;
            if index > 1 {
                let call = format!("call_patch_{}", index - 1);
                let output = body["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| {
                        item["type"] == "custom_tool_call_output" && item["call_id"] == call
                    })
                    .expect("actual native result");
                assert!(output["output"].to_string().contains(match index {
                    2 => "Success",
                    3 => "missing.txt",
                    _ => "blocker",
                }));
            }
            match index {
                1 => reply(index, gate),
                2 => {
                    assert_eq!(
                        std::fs::read_to_string(project.join("input.txt")).unwrap(),
                        "keep\nafter\n"
                    );
                    assert!(
                        std::fs::read_to_string(project.join("added.txt"))
                            .unwrap()
                            .contains("R2_PATCH_ADDED")
                    );
                    assert_eq!(
                        std::fs::read_to_string(project.join("moved.txt")).unwrap(),
                        "R2_PATCH_MOVED\n"
                    );
                    assert!(!project.join("move-from.txt").exists());
                    assert!(!project.join("delete.txt").exists());
                    gate.notified().await;
                    reply(index, gate)
                }
                3 => {
                    assert!(!project.join("missing.txt").exists());
                    reply(index, gate)
                }
                4 => {
                    assert!(!project.join("blocker/child.txt").exists());
                    assert_eq!(
                        std::fs::read_to_string(project.join("blocker")).unwrap(),
                        "existing file\n"
                    );
                    interaction::final_message(
                        4,
                        "R2_PATCH_DONE：拟议修改与原生成功、执行失败、校验错误分别可读。",
                    )
                }
                _ => panic!("unexpected native request or replay"),
            }
        }
    })
    .await;
    let config = format!(
        r#"model="gpt-5.5"
model_provider="custom"
check_for_update_on_startup=false
approval_policy="never"
sandbox_mode="workspace-write"
[model_providers.custom]
name="Synthetic R2 patch"
base_url="http://{}/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-r2-patch"
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
    let mut browser = tokio::process::Command::new("node");
    browser
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r2-patch-probe.cjs"))
        .env("WORKBENCH_PROBE_URL", &entry.url)
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
    let mut stages = Vec::new();
    timeout(Duration::from_secs(90), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let report: Value = serde_json::from_str(&line).unwrap();
            println!("{report}");
            if matches!(report["stage"].as_str(), Some("intermediate" | "proposal")) {
                stages.push(report["stage"].as_str().unwrap().to_owned());
                gate.notify_one();
            }
        }
        assert!(
            browser.wait().await.unwrap().success(),
            "native patch browser assertions failed"
        );
    })
    .await
    .unwrap();
    assert_eq!(stages, ["intermediate", "proposal"]);
    assert_eq!(count.load(Ordering::SeqCst), 4);
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
    let records: Vec<_> = super::rollout_tools::native_items(&home)
        .into_iter()
        .filter(|event| event["item"]["type"] == "FileChange")
        .collect();
    for event in &records {
        println!(
            "{}",
            json!({"check":"native-file-change-shape","callId":event["item"]["id"],"status":event["item"]["status"],"itemKeys":event["item"].as_object().unwrap().keys().collect::<Vec<_>>(),"changes":event["item"]["changes"].as_object().map(|changes| changes.values().map(|change|change.as_object().unwrap().keys().cloned().collect::<Vec<_>>()).collect::<Vec<_>>()),"stdoutPresent":event["item"]["stdout"].is_string(),"stderrPresent":event["item"]["stderr"].is_string(),"threadIdPresent":event["thread_id"].is_string(),"turnIdPresent":event["turn_id"].is_string()})
        );
    }
    assert!(records.iter().any(
        |event| event["item"]["id"] == "call_patch_1" && event["item"]["status"] == "completed"
    ));
    assert!(
        records
            .iter()
            .any(|event| event["item"]["id"] == "call_patch_3"
                && event["item"]["status"] == "failed")
    );
    assert!(
        !records
            .iter()
            .any(|event| event["item"]["id"] == "call_patch_2")
    );
    println!(
        "{}",
        json!({"check":"r2-native-patch","add":true,"update":true,"move":true,"delete":true,"validationFailure":true,"nativeExecutionFailure":true,"mainRequests":4,"configurationPreserved":true})
    );
}
