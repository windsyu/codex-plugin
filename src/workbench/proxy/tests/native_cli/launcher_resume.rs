//! Explicit native resume through two actual product runs; no automatic task replay.
use super::launcher::{LauncherChild, launcher_command, wait_entry};
use super::*;
use std::path::Path;
use std::process::Stdio;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and Chrome; synthetic durable history only"]
async fn product_launcher_explicit_resume_keeps_native_history_without_replaying_input() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("resume-project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let thread = Arc::new(Mutex::new(None::<Uuid>));
    let (seen, native_thread) = (count.clone(), thread.clone());
    let upstream = fixture(move |request| {
        let (seen, native_thread) = (seen.clone(), native_thread.clone());
        async move {
            assert_eq!(
                request.headers().get(header::AUTHORIZATION).unwrap(),
                "Bearer synthetic-launcher-resume"
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
            let index = seen.fetch_add(1, Ordering::SeqCst);
            let metadata: Value = serde_json::from_str(
                body["client_metadata"]["x-codex-turn-metadata"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            let id = Uuid::parse_str(metadata["thread_id"].as_str().unwrap()).unwrap();
            assert_eq!(body["model"], "gpt-6-astra");
            assert!(body["input"].to_string().contains("R1_RESUME_FIRST"));
            match index {
                0 => {
                    *native_thread.lock().unwrap() = Some(id);
                    interaction::final_message(0, "R1_RESUME_HISTORY")
                }
                1 => {
                    assert_eq!(*native_thread.lock().unwrap(), Some(id));
                    assert!(body["input"].to_string().contains("R1_RESUME_HISTORY"));
                    assert!(body["input"].to_string().contains("R1_RESUME_NEXT"));
                    interaction::final_message(1, "R1_RESUME_DONE")
                }
                _ => panic!("resume must never replay an earlier model request"),
            }
        }
    })
    .await;
    let config = format!(
        r#"model="gpt-6-astra"
model_provider="custom"
check_for_update_on_startup=false
[model_providers.custom]
name="Synthetic launcher resume"
base_url="http://{}/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-launcher-resume"
"#,
        upstream.address
    );
    std::fs::write(home.join("config.toml"), &config).unwrap();
    let mut previous_epoch = None;
    for phase in 0..2 {
        let entry_file = directory.path().join(format!("entry-{phase}.json"));
        let mut command = launcher_command(&home, &workspace, &entry_file);
        if phase == 1 {
            command.args(["--resume", &thread.lock().unwrap().unwrap().to_string()]);
        }
        let mut child = LauncherChild(command.spawn().unwrap());
        let entry = wait_entry(&mut child, &entry_file).await;
        assert_ne!(previous_epoch, Some(entry.run_epoch));
        previous_epoch = Some(entry.run_epoch);
        assert_eq!(count.load(Ordering::SeqCst), phase);
        let mut browser = tokio::process::Command::new("node");
        browser
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r1-resume-probe.cjs"))
            .env("WORKBENCH_PROBE_URL", &entry.url)
            .env(
                "WORKBENCH_PROBE_RESUMED",
                if phase == 1 { "true" } else { "false" },
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
            "product resume browser probe failed"
        );
        assert_eq!(count.load(Ordering::SeqCst), phase + 1);
        if phase == 0 {
            let native_id = timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(id) = resume::session_evidence(&home, "R1_RESUME_HISTORY") {
                        break id;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(*thread.lock().unwrap(), Some(native_id));
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
    }
    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
    let before: toml::Value = toml::from_str(&config).unwrap();
    assert_eq!(after["model_providers"], before["model_providers"]);
    assert_eq!(after["model"], before["model"]);
    println!(
        "{}",
        json!({"check":"product-launcher-resume","mainRequests":2,"sameNativeThread":true,"newRunEpoch":true,"nativeHistoryInNextRequest":true,"noAutomaticReplay":true})
    );
}
