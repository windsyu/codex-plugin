//! Same-turn native steering while an observed model response is still open.
use super::launcher::{LauncherChild, launcher_command, wait_entry};
use super::*;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Notify;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and Chrome; synthetic native steering only"]
async fn native_steering_keeps_both_submissions_and_same_turn_without_optimistic_messages() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("R2-steering-project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    let gate = Arc::new(Notify::new());
    let domains = Arc::new(Mutex::new(Vec::<Value>::new()));
    let (model_gate, observed) = (gate.clone(), domains.clone());
    let upstream = fixture(move |request| {
        let (gate, observed) = (model_gate.clone(), observed.clone());
        async move {
            let body: Value = serde_json::from_slice(&axum::body::to_bytes(request.into_body(), 2*1024*1024).await.unwrap()).unwrap();
            if let Some(title) = title_response(&body) { return title; }
            let meta: Value = serde_json::from_str(body["client_metadata"]["x-codex-turn-metadata"].as_str().unwrap()).unwrap();
            assert_eq!(meta["request_kind"], "turn"); assert_eq!(meta["thread_source"], "user");
            let mut domains = observed.lock().unwrap(); let index = domains.len();
            if index == 1 {
                assert_eq!(meta["thread_id"], domains[0]["thread_id"]);
                assert_eq!(meta["turn_id"], domains[0]["turn_id"], "steering must remain in the active native turn");
                assert!(body["input"].to_string().contains("R2_STEER_ADDED"));
            }
            domains.push(json!({"thread_id":meta["thread_id"],"turn_id":meta["turn_id"]}));
            assert!(index < 2, "unexpected steering replay"); drop(domains);
            if index == 1 { return interaction::final_message(2, "R2_STEER_FINISHED：追加输入已进入当前轮次。"); }
            let stream = async_stream::stream! {
                yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":"response-steering"}})));
                yield Ok(event(json!({"type":"response.output_text.delta","item_id":"message-steering","content_index":0,"delta":"R2_STEER_PARTIAL：等待追加输入。"})));
                gate.notified().await;
                yield Ok(event(json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","id":"message-steering","content":[{"type":"output_text","text":"R2_STEER_FIRST_COMPLETE：第一段回复结束。"}]}})));
                yield Ok(event(json!({"type":"response.completed","response":{"id":"response-steering"}})));
            };
            Response::builder().header(header::CONTENT_TYPE,"text/event-stream").body(Body::from_stream(stream)).unwrap()
        }
    }).await;
    std::fs::write(
        home.join("config.toml"),
        format!(
            r#"model="gpt-5.5"
model_provider="custom"
check_for_update_on_startup=false
[model_providers.custom]
name="Synthetic steering"
base_url="http://{}/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-r2-steering"
"#,
            upstream.address
        ),
    )
    .unwrap();
    let entry_file = directory.path().join("entry.json");
    let mut child = LauncherChild(
        launcher_command(&home, &workspace, &entry_file)
            .spawn()
            .unwrap(),
    );
    let entry = wait_entry(&mut child, &entry_file).await;
    let mut command = tokio::process::Command::new("node");
    command
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r2-steering-probe.cjs"))
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
                "steer-submitted" => gate.notify_one(),
                "complete" => complete = true,
                _ => panic!("steering browser failed at safe stage"),
            }
        }
        assert!(browser.wait().await.unwrap().success());
    })
    .await
    .unwrap();
    assert!(complete);
    assert_eq!(domains.lock().unwrap().len(), 2);
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
    let records = rollout_tools::native_items(&home);
    let users: Vec<_> = records
        .iter()
        .filter(|event| event["item"]["type"] == "UserMessage")
        .collect();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0]["turn_id"], users[1]["turn_id"]);
    assert_ne!(users[0]["item"]["id"], users[1]["item"]["id"]);
    assert_eq!(users[0]["turn_id"], domains.lock().unwrap()[0]["turn_id"]);
    assert!(!entry_file.exists());
    println!(
        "{}",
        json!({"check":"native-steering","nativeSubmissions":2,"modelRequests":2,"sameNativeTurn":true,"distinctNativeItems":true})
    );
}
