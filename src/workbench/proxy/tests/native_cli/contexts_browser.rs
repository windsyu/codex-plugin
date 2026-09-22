//! Native compact, fork and explicit resume through the product and real Chrome.
//! Request history is fixture data; no private model or Codex home is used.
use super::launcher::{LauncherChild, launcher_command, wait_entry};
use super::*;
use std::path::Path;
use std::process::Stdio;

const PROMPT: &str = "R2_CONTEXT_INPUT：同一句提交，保留合成上下文。";
const SUMMARY: &str = "R2_COMPACT_SUMMARY：这是上下文摘要，不是新的用户提交。\n<script>window.contextInjected=1</script>";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and Chrome; synthetic compact/fork/resume only"]
async fn native_compact_fork_and_resume_keep_context_out_of_new_user_bubbles() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("R2-context-project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let compactions = Arc::new(AtomicUsize::new(0));
    let (observed, compact_count) = (requests.clone(), compactions.clone());
    let upstream = fixture(move |request| {
        let (observed, compact_count) = (observed.clone(), compact_count.clone());
        async move {
            assert_eq!(request.uri().path(), "/v1/responses");
            assert_eq!(request.headers().get(header::AUTHORIZATION).unwrap(), "Bearer synthetic-r2-context");
            let body: Value = serde_json::from_slice(&axum::body::to_bytes(request.into_body(), 2 * 1024 * 1024).await.unwrap()).unwrap();
            let metadata: Value = serde_json::from_str(body["client_metadata"]["x-codex-turn-metadata"].as_str().unwrap()).unwrap();
            if let Some(title) = title_response(&body) { return title; }
            assert_eq!(metadata["thread_source"], "user");
            if metadata["request_kind"] == "compaction" {
                assert_eq!(compact_count.fetch_add(1, Ordering::SeqCst), 0);
                assert!(body["input"].to_string().contains(PROMPT));
                return interaction::final_message(100, SUMMARY);
            }
            assert_eq!(metadata["request_kind"], "turn");
            let mut requests = observed.lock().unwrap();
            let index = requests.len();
            let input = body["input"].to_string();
            assert!(input.contains(PROMPT));
            if index > 0 { assert!(input.contains("R2_COMPACT_SUMMARY"), "native compacted history must survive fork/resume"); }
            match index {
                0 => {}
                1 => { assert_eq!(metadata["thread_id"], requests[0]["thread_id"]); assert_ne!(metadata["turn_id"], requests[0]["turn_id"]); }
                2 => { assert_ne!(metadata["thread_id"], requests[0]["thread_id"]); assert!(input.contains("R2_CONTEXT_REPLY_2")); }
                3 => { assert_eq!(metadata["thread_id"], requests[2]["thread_id"]); assert!(input.contains("R2_CONTEXT_REPLY_3")); }
                _ => panic!("unexpected context submission or automatic replay"),
            }
            requests.push(json!({"thread_id":metadata["thread_id"],"turn_id":metadata["turn_id"],"inputItems":body["input"].as_array().unwrap().len(),"containsSummary":input.contains("R2_COMPACT_SUMMARY")}));
            interaction::final_message(index + 1, &format!("R2_CONTEXT_REPLY_{}", index + 1))
        }
    }).await;
    let config = format!(
        r#"model="gpt-5.5"
model_provider="custom"
check_for_update_on_startup=false
[model_providers.custom]
name="Synthetic R2 context"
base_url="http://{}/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-r2-context"
"#,
        upstream.address
    );
    std::fs::write(home.join("config.toml"), &config).unwrap();
    let mut previous_epoch = None;
    for resumed in [false, true] {
        let entry_file = directory.path().join(format!("entry-{resumed}.json"));
        let mut command = launcher_command(&home, &workspace, &entry_file);
        if resumed {
            command.args([
                "--resume",
                requests.lock().unwrap()[2]["thread_id"].as_str().unwrap(),
            ]);
        }
        let mut child = LauncherChild(command.spawn().unwrap());
        let entry = wait_entry(&mut child, &entry_file).await;
        assert_ne!(previous_epoch, Some(entry.run_epoch));
        previous_epoch = Some(entry.run_epoch);
        let mut browser = tokio::process::Command::new("node");
        browser
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r2-context-history-probe.cjs"))
            .env("WORKBENCH_PROBE_URL", &entry.url)
            .env(
                "WORKBENCH_PROBE_RESUMED",
                if resumed { "true" } else { "false" },
            )
            .env("DEBUG", "")
            .env("PWDEBUG", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(path) = std::env::var_os("WORKBENCH_TEST_SCREENSHOT") {
            browser.env("WORKBENCH_PROBE_SCREENSHOT", path);
        }
        let mut browser =
            crate::workbench::probe_process::ProbeProcess::spawn(&mut browser).unwrap();
        let _liveness = browser.stdin.take();
        assert!(
            timeout(Duration::from_secs(65), browser.wait())
                .await
                .unwrap()
                .unwrap()
                .success(),
            "native context browser check failed; private URL/output suppressed"
        );
        assert_eq!(requests.lock().unwrap().len(), if resumed { 4 } else { 3 });
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
    }
    assert_eq!(compactions.load(Ordering::SeqCst), 1);
    let records = rollout_tools::native_items(&home);
    let users: Vec<_> = records
        .iter()
        .filter(|event| event["item"]["type"] == "UserMessage")
        .collect();
    assert_eq!(
        users.len(),
        4,
        "native compact/fork/resume must not add artificial human submissions"
    );
    for request in requests.lock().unwrap().iter() {
        assert_eq!(
            users
                .iter()
                .filter(|event| event["thread_id"] == request["thread_id"]
                    && event["turn_id"] == request["turn_id"])
                .count(),
            1
        );
    }
    let before: toml::Value = toml::from_str(&config).unwrap();
    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
    assert_eq!(before["model_providers"], after["model_providers"]);
    println!(
        "{}",
        json!({"check":"native-context-history","conversationRequests":4,"compactionRequests":1,"nativeSubmissions":4,"forkChangesThread":true,"resumeKeepsFork":true,"configurationPreserved":true})
    );
}
