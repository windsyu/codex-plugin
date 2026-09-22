use super::launcher::{LauncherChild, launcher_command, wait_entry};
use super::*;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Notify;

pub(super) fn advertises(body: &Value, name: &str) -> bool {
    fn list(value: &Value, name: &str) -> bool {
        value.as_array().is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool["name"] == name || (tool["type"] == "namespace" && list(&tool["tools"], name))
            })
        })
    }
    list(&body["tools"], name)
        || body["input"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["type"] == "additional_tools"
                    && item["role"] == "developer"
                    && list(&item["tools"], name)
            })
        })
}
fn response(index: usize, code: bool, arguments: Value, gate: Arc<Notify>) -> Response {
    let raw = if code {
        format!("text(await tools.exec_command({arguments}));")
    } else {
        arguments.to_string()
    };
    let item = if code {
        json!({"id":format!("tool_r2_{index}"),"call_id":format!("call_r2_{index}"),"type":"custom_tool_call","name":"exec","input":raw})
    } else {
        json!({"id":format!("tool_r2_{index}"),"call_id":format!("call_r2_{index}"),"type":"function_call","name":"exec_command","arguments":raw})
    };
    let mut initial = item.clone();
    initial[if code { "input" } else { "arguments" }] = json!("");
    let stream = async_stream::stream! {
        let id=format!("resp_r2_{index}");
        let intro=if index==1 {"R2_TOOL_INTRO：先读取合成文件。\n\n```sh\necho 这只是模型文字中的代码示例\n```"} else if index==2 {"R2_READ_FINISHED：接着修改临时文件。"} else {"R2_EDIT_FINISHED：最后验证非零退出码。"};
        yield Ok::<_,std::io::Error>(event(json!({"type":"response.created","response":{"id":id}})));
        yield Ok(event(json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","id":format!("msg_r2_{index}"),"content":[{"type":"output_text","text":intro}]}})));
        yield Ok(event(json!({"type":"response.output_item.added","output_index":1,"item":initial})));
        let characters:Vec<_>=raw.chars().collect();
        for (chunk_index,chunk) in characters.chunks(12).enumerate() {
            yield Ok(event(json!({"type":if code {"response.custom_tool_call_input.delta"} else {"response.function_call_arguments.delta"},"item_id":format!("tool_r2_{index}"),"delta":chunk.iter().collect::<String>()})));
            if index==1 && chunk_index==2 { gate.notified().await; }
            tokio::time::sleep(Duration::from_millis(12)).await;
        }
        yield Ok(event(json!({"type":"response.output_item.done","output_index":1,"item":item})));
        yield Ok(event(json!({"type":"response.completed","response":{"id":id}})));
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and Chrome; synthetic read/edit/failure only"]
async fn native_code_and_command_cards_match_actual_read_edit_and_failed_results() {
    for code in [true, false] {
        scenario(code).await;
    }
}
async fn scenario(code: bool) {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join(if code {
        "R2-code-tools"
    } else {
        "R2-command-tools"
    });
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(
        workspace.join("input.txt"),
        "R2_READ_OK\n<svg onload=\"window.r2Injected=1\">literal</svg>\n",
    )
    .unwrap();
    let gate = Arc::new(Notify::new());
    let server_gate = gate.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let project = workspace.clone();
    let upstream=fixture(move|request|{
        let (gate,seen,project)=(server_gate.clone(),seen.clone(),project.clone());
        async move {
            let body:Value=serde_json::from_slice(&axum::body::to_bytes(request.into_body(),2*1024*1024).await.unwrap()).unwrap();
            if let Some(title)=title_response(&body){return title;}
            let index=seen.fetch_add(1,Ordering::SeqCst)+1;
            assert!(advertises(&body,if code {"exec"} else {"exec_command"}),"native tool must be present in the actual request definitions");
            if index>1 {
                let expected=match index {2=>"R2_READ_OK",3=>"R2_EDIT_OK",_=>"R2_FAIL_OK"};
                let call_id=format!("call_r2_{}",index-1);
                let output=body["input"].as_array().unwrap().iter().find(|item|item["call_id"]==call_id && item["type"]==if code {"custom_tool_call_output"} else {"function_call_output"}).expect("matching real native output");
                assert!(output["output"].to_string().contains(expected),"native output must contain the actual file/command marker");
            }
            match index {
                1..=3 => {
                    let cmd=match index {1=>"cat input.txt",2=>"printf 'R2_EDIT_OK\\n' > result.txt; cat result.txt",_=>"printf 'R2_FAIL_OK\\n'; exit 7"};
                    response(index,code,json!({"cmd":cmd,"workdir":project,"login":false,"max_output_tokens":500,"yield_time_ms":1000}),gate)
                }
                4 => {
                    assert_eq!(std::fs::read_to_string(project.join("result.txt")).unwrap(),"R2_EDIT_OK\n");
                    interaction::final_message(4,"R2_TOOLS_DONE：读取、修改和失败结果已分别观察；工具参数生成与执行事实分开呈现。")
                }
                _=>panic!("unexpected native request or replay"),
            }
        }
    }).await;
    let config = format!(
        r#"model="{}"
model_provider="custom"
check_for_update_on_startup=false
approval_policy="never"
sandbox_mode="workspace-write"
[model_providers.custom]
name="Synthetic R2 tools"
base_url="http://{}/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-r2-tools"
[features]
code_mode={code}
code_mode_only=false
"#,
        if code { "gpt-6-astra" } else { "gpt-5.5" },
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
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r2-tools-probe.cjs"))
        .env("WORKBENCH_PROBE_URL", &entry.url)
        .env("WORKBENCH_PROBE_CODE", if code { "true" } else { "false" })
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
    let mut intermediate = false;
    timeout(Duration::from_secs(90), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let report: Value = serde_json::from_str(&line).unwrap();
            println!("{report}");
            if report["stage"] == "intermediate" {
                intermediate = true;
                gate.notify_one();
            }
        }
        assert!(
            browser.wait().await.unwrap().success(),
            "native R2 browser assertions failed"
        );
    })
    .await
    .unwrap();
    assert!(intermediate);
    assert_eq!(count.load(Ordering::SeqCst), 4);
    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
    let before: toml::Value = toml::from_str(&config).unwrap();
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
    println!(
        "{}",
        json!({"check":"r2-native-tools","codeMode":code,"mainRequests":4,"fileRead":true,"fileEdited":true,"failedCommandReturned":true,"configurationPreserved":true})
    );
}
