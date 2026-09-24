//! R6-E homepage → explicit new/resume → real native CLI, with a synthetic model.
use super::launcher::LauncherChild;
use super::*;
use std::path::Path;
use std::process::Stdio;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and system Chrome; isolated homes and synthetic upstream"]
async fn product_homepage_launches_new_and_resumes_native_session_without_replay() {
    homepage_launch(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and system Chrome; isolated API-key auth and synthetic upstream"]
async fn product_homepage_launches_openai_auth_custom_bearer_with_ambient_api_key() {
    homepage_launch(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and system Chrome; isolated projectless Desktop fixture"]
async fn product_homepage_resumes_unassigned_native_session_in_recorded_directory() {
    homepage_launch(false, true).await;
}

fn authenticated_reply() -> Response {
    let stream = async_stream::stream! {
        let response = "resp_auth_home_0";
        let item = "msg_auth_home_0";
        yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":response,"model":"gpt-6-astra"}})));
        yield Ok(event(json!({"type":"response.output_item.added","item":{"id":item,"type":"message","role":"assistant","content":[]}})));
        // Delimit the marker: a trailing D alone is a possible data: URI prefix
        // and is intentionally held by streaming redaction until the next byte.
        let first = "R6_E_HISTORY synthetic-r6-launch synthetic-ambient-api-key R6_E_REDACTED.";
        for chunk in first.chars().collect::<Vec<_>>().chunks(7) {
            yield Ok(event(json!({"type":"response.output_text.delta","item_id":item,"content_index":0,"delta":chunk.iter().collect::<String>()})));
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        // The browser must observe the redacted intermediate text before this final marker.
        tokio::time::sleep(Duration::from_secs(4)).await;
        let tail = " R6_E_STREAM_DONE";
        yield Ok(event(json!({"type":"response.output_text.delta","item_id":item,"content_index":0,"delta":tail})));
        yield Ok(event(json!({"type":"response.output_item.done","item":{"id":item,"type":"message","role":"assistant","content":[{"type":"output_text","text":format!("{first}{tail}")}]}})));
        yield Ok(event(json!({"type":"response.completed","response":{"id":response}})));
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

fn verify_recording_redaction(root: &Path) -> (usize, bool) {
    let mut count = 0;
    let mut observed_reply = false;
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            let (nested, observed) = verify_recording_redaction(&path);
            count += nested;
            observed_reply |= observed;
        } else if path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("observations.")
        {
            let bytes = std::fs::read(&path).unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert!(
                !text.contains("synthetic-r6-launch"),
                "provider secret in observation journal"
            );
            assert!(
                !text.contains("synthetic-ambient-api-key"),
                "ambient API key in observation journal"
            );
            observed_reply |= text.contains("R6_E_REDACTED");
            count += 1;
        }
    }
    (count, observed_reply)
}

async fn homepage_launch(openai_auth: bool, projectless: bool) {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("native");
    let workspace = directory.path().join(if projectless {
        "Documents/Codex/2026-09-23/new-chat"
    } else {
        "项目 with spaces"
    });
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("R6-check.txt"), "Synthetic R6 source\n").unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let identity = Arc::new(Mutex::new(None::<Uuid>));
    let (seen, thread) = (count.clone(), identity.clone());
    let upstream = fixture(move |request| {
        let (seen, thread) = (seen.clone(), thread.clone());
        async move {
            assert_eq!(
                request.headers().get(header::AUTHORIZATION).unwrap(),
                "Bearer synthetic-r6-launch"
            );
            assert!(request.headers().get("ChatGPT-Account-ID").is_none());
            let body: Value = serde_json::from_slice(
                &axum::body::to_bytes(request.into_body(), 2 * 1024 * 1024)
                    .await
                    .unwrap(),
            )
            .unwrap();
            if let Some(title) = title_response(&body) {
                return title;
            }
            let metadata: Value = serde_json::from_str(
                body["client_metadata"]["x-codex-turn-metadata"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            let id = Uuid::parse_str(metadata["thread_id"].as_str().unwrap()).unwrap();
            let index = seen.fetch_add(1, Ordering::SeqCst);
            assert!(body["input"].to_string().contains("R6_E_FIRST"));
            match index {
                0 => {
                    *thread.lock().unwrap() = Some(id);
                    if openai_auth {
                        authenticated_reply()
                    } else {
                        interaction::final_message(0, "R6_E_HISTORY")
                    }
                }
                1 => {
                    assert_eq!(*thread.lock().unwrap(), Some(id));
                    assert!(body["input"].to_string().contains("R6_E_HISTORY"));
                    assert!(body["input"].to_string().contains("R6_E_NEXT"));
                    interaction::final_message(1, "R6_E_DONE")
                }
                _ => panic!("unexpected automatic model turn"),
            }
        }
    })
    .await;
    let config = format!(
        "model=\"gpt-6-astra\"\nmodel_provider=\"custom\"\ncheck_for_update_on_startup=false\n[model_providers.custom]\nname=\"Synthetic R6 launch\"\nbase_url=\"http://{}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth={openai_auth}\nexperimental_bearer_token=\"synthetic-r6-launch\"\n",
        upstream.address
    );
    std::fs::write(home.join("config.toml"), &config).unwrap();
    let auth = r#"{"auth_mode":"apikey","OPENAI_API_KEY":"synthetic-ambient-api-key"}"#;
    if openai_auth {
        std::fs::write(home.join("auth.json"), auth).unwrap();
    }
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
        .env_remove("CODEX_INTERNAL_ORIGINATOR_OVERRIDE")
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
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join(if openai_auth {
            "web/e2e/r6-auth-launch-probe.cjs"
        } else {
            "web/e2e/r6-launch-probe.cjs"
        }))
        .env("HOME", directory.path())
        .env("USERPROFILE", directory.path())
        .env("CODEX_HOME", &home)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .env_remove("CODEX_INTERNAL_ORIGINATOR_OVERRIDE")
        .env(
            "WORKBENCH_PROBE_PROJECTLESS",
            if projectless { "1" } else { "0" },
        )
        .env("WORKBENCH_PROBE_ISOLATED_ROOT", directory.path())
        .env("WORKBENCH_PROBE_URL", &entry.url)
        .env("WORKBENCH_PROBE_PROJECT", workspace.canonicalize().unwrap())
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
        timeout(Duration::from_secs(105), browser.wait())
            .await
            .unwrap()
            .unwrap()
            .success(),
        "R6 homepage native browser probe failed"
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(identity.lock().unwrap().is_some());
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
    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
    let before: toml::Value = toml::from_str(&config).unwrap();
    assert_eq!(after["model_providers"], before["model_providers"]);
    assert_eq!(after["model"], before["model"]);
    assert_eq!(after["model_provider"], before["model_provider"]);
    if openai_auth {
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            auth
        );
        let (journals, observed) =
            verify_recording_redaction(&directory.path().join(".codex-web/history"));
        assert!(journals >= 2, "both runs must record observations");
        assert!(
            observed,
            "redaction check must include the actual streamed reply"
        );
    }
}
