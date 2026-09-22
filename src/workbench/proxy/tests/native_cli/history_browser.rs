//! R3 through two real product processes and the installed native CLI.
use super::launcher::{LauncherChild, launcher_command, wait_entry};
use super::*;
use std::path::Path;
use std::process::Stdio;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and Chrome; temporary history and synthetic upstream only"]
async fn product_history_survives_restart_without_resuming_or_replaying_native_input() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("R3-history-project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let project = workspace.clone();
    let threads = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = threads.clone();
    let upstream=fixture(move|request|{
        let seen=seen.clone();let project=project.clone();let observed=observed.clone();
        async move {
            let body:Value=serde_json::from_slice(&axum::body::to_bytes(request.into_body(),2*1024*1024).await.unwrap()).unwrap();
            if let Some(title)=title_response(&body){return title;}
            let index=seen.fetch_add(1,Ordering::SeqCst);
            let metadata:Value=serde_json::from_str(body["client_metadata"]["x-codex-turn-metadata"].as_str().unwrap()).unwrap();
            observed.lock().unwrap().push(metadata["thread_id"].as_str().unwrap().into());
            match index {
                0=>{
                    assert!(tools_browser::advertises(&body,"exec_command"));assert!(body["input"].to_string().contains("R3_SEED"));
                    let call=json!({"type":"function_call","id":"r3_command","call_id":"call_r3_history","name":"exec_command","arguments":json!({"cmd":"printf 'R3_NATIVE_TOOL\\n'","workdir":project,"login":false,"max_output_tokens":100,"yield_time_ms":1000}).to_string()});
                    Response::builder().header(header::CONTENT_TYPE,"text/event-stream").body(Body::from([
                        event(json!({"type":"response.created","response":{"id":"r3_tool_response"}})),
                        event(json!({"type":"response.output_item.done","output_index":0,"item":call})),
                        event(json!({"type":"response.completed","response":{"id":"r3_tool_response"}}))
                    ].concat())).unwrap()
                }
                1=>{
                    assert!(body["input"].to_string().contains("R3_NATIVE_TOOL"));
                    interaction::final_message(301,"R3_SAVED_REPLY：这是已保存的合成回复。\n\nsynthetic-history-token\n\n<svg onload=\"window.r3Injected=1\">literal</svg>")
                }
                2=>{
                    assert!(body["input"].to_string().contains("R3_CURRENT"));assert!(!body["input"].to_string().contains("R3_SEED"));
                    let text=format!("R3_CURRENT_REPLY\n\n{}\n\nR3_CURRENT_REPLY",(0..35).map(|i|format!("{i}. 当前运行保持原生输入，历史只用于阅读，返回后保留阅读位置。\n\n")).collect::<String>());
                    interaction::final_message(302,&text)
                }
                _=>panic!("history must never replay a native request"),
            }
        }
    }).await;
    let config = format!(
        r#"model="gpt-5.5"
model_provider="custom"
check_for_update_on_startup=false
approval_policy="never"
sandbox_mode="workspace-write"
[features]
code_mode=false
code_mode_only=false
[model_providers.custom]
name="Synthetic R3 history"
base_url="http://{}/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-history-token"
"#,
        upstream.address
    );
    std::fs::write(home.join("config.toml"), config).unwrap();
    let mut old_epoch = None;
    for phase in 0..2 {
        let entry_file = dir.path().join(format!("entry-{phase}.json"));
        let mut launcher = LauncherChild(
            launcher_command(&home, &workspace, &entry_file)
                .spawn()
                .unwrap(),
        );
        let entry = wait_entry(&mut launcher, &entry_file).await;
        assert_eq!(count.load(Ordering::SeqCst), if phase == 0 { 0 } else { 2 });
        let mut command = tokio::process::Command::new("node");
        command
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r3-history-probe.cjs"))
            .env("WORKBENCH_PROBE_URL", &entry.url)
            .env(
                "WORKBENCH_PROBE_OLD_EPOCH",
                old_epoch.map_or(String::new(), |id: Uuid| id.to_string()),
            )
            .env("DEBUG", "")
            .env("PWDEBUG", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(path) = std::env::var_os("WORKBENCH_TEST_SCREENSHOT") {
            command.env("WORKBENCH_PROBE_SCREENSHOT", path);
        }
        let mut browser =
            crate::workbench::probe_process::ProbeProcess::spawn(&mut command).unwrap();
        let _liveness = browser.stdin.take();
        assert!(
            timeout(Duration::from_secs(65), browser.wait())
                .await
                .unwrap()
                .unwrap()
                .success(),
            "history browser probe failed (private output suppressed)"
        );
        assert_eq!(count.load(Ordering::SeqCst), if phase == 0 { 2 } else { 3 });
        assert_eq!(
            unsafe { libc::kill(launcher.0.id().unwrap() as i32, libc::SIGTERM) },
            0
        );
        assert!(
            timeout(Duration::from_secs(5), launcher.0.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        assert!(!entry_file.exists());
        old_epoch = Some(entry.run_epoch);
    }
    let ids = threads.lock().unwrap();
    assert_eq!(ids[0], ids[1]);
    assert_ne!(ids[1], ids[2]);
    println!(
        "{}",
        json!({"check":"r3-product-history","modelRequests":3,"nativeCommandExecutions":1,"independentRuns":2,"historyDidNotResumeOrReplay":true})
    );
}
