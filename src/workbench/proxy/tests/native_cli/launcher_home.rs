//! R6-E homepage → explicit new/resume → real native CLI, with a synthetic model.
use super::launcher::LauncherChild;
use super::*;
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthMode {
    StaticBearer,
    NativeApiKey,
    Command,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and system Chrome; isolated homes and synthetic upstream"]
async fn product_homepage_launches_new_and_resumes_native_session_without_replay() {
    homepage_launch(AuthMode::StaticBearer, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and system Chrome; isolated API-key auth and synthetic upstream"]
async fn product_homepage_launches_openai_auth_custom_bearer_with_ambient_api_key() {
    homepage_launch(AuthMode::NativeApiKey, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and system Chrome; isolated command auth and synthetic upstream"]
async fn product_homepage_leaves_command_auth_and_401_refresh_to_native_cli() {
    homepage_launch(AuthMode::Command, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built codex-view, installed CLI and system Chrome; isolated projectless Desktop fixture"]
async fn product_homepage_resumes_unassigned_native_session_in_recorded_directory() {
    homepage_launch(AuthMode::StaticBearer, true).await;
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

fn verify_recording_redaction(root: &Path, reply_marker: &str) -> (usize, bool) {
    let mut count = 0;
    let mut observed_reply = false;
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            let (nested, observed) = verify_recording_redaction(&path, reply_marker);
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
            for forbidden in [
                "synthetic-command-first",
                "synthetic-command-second",
                "synthetic-provider-auth.sh",
            ] {
                assert!(
                    !text.contains(forbidden),
                    "provider command or credential in observation journal"
                );
            }
            observed_reply |= text.contains(reply_marker);
            count += 1;
        }
    }
    (count, observed_reply)
}

async fn homepage_launch(auth_mode: AuthMode, projectless: bool) {
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
    let auth_script = directory.path().join("synthetic-provider-auth.sh");
    let auth_calls = directory.path().join("synthetic-provider-auth.sh.calls");
    let auth_rotate = directory.path().join("synthetic-provider-auth.sh.rotate");
    if auth_mode == AuthMode::Command {
        std::fs::write(
            &auth_script,
            "#!/bin/sh\nprintf 'call\\n' >> \"$0.calls\"\nif [ -f \"$0.rotate\" ]; then printf 'synthetic-command-second'; else printf 'synthetic-command-first'; fi\n",
        )
        .unwrap();
        std::fs::set_permissions(&auth_script, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let count = Arc::new(AtomicUsize::new(0));
    let identity = Arc::new(Mutex::new(None::<Uuid>));
    let first_command_token_seen = Arc::new(AtomicBool::new(false));
    let rejected_command_routes = Arc::new(Mutex::new(HashSet::<String>::new()));
    let refreshed_rejected_command_route = Arc::new(AtomicBool::new(false));
    let (seen, thread) = (count.clone(), identity.clone());
    let command_first = first_command_token_seen.clone();
    let rejected_routes = rejected_command_routes.clone();
    let refreshed_route = refreshed_rejected_command_route.clone();
    let rotate_file = auth_rotate.clone();
    let upstream = fixture(move |request| {
        let (seen, thread, command_first, rejected_routes, refreshed_route, rotate_file) = (
            seen.clone(),
            thread.clone(),
            command_first.clone(),
            rejected_routes.clone(),
            refreshed_route.clone(),
            rotate_file.clone(),
        );
        async move {
            let route = format!(
                "{} {}",
                request.method(),
                request
                    .uri()
                    .path_and_query()
                    .map(|value| value.as_str())
                    .unwrap_or_else(|| request.uri().path())
            );
            let is_models = request.uri().path().ends_with("/models");
            if auth_mode == AuthMode::Command
                && request.headers().get(header::AUTHORIZATION)
                    == Some(&header::HeaderValue::from_static(
                        "Bearer synthetic-command-first",
                    ))
            {
                // Catalog traffic must not consume the stale-token scenario:
                // accept it until an actual model request gets the first 401.
                if is_models {
                    return axum::Json(json!({"models": []})).into_response();
                }
                command_first.store(true, Ordering::SeqCst);
                rejected_routes.lock().unwrap().insert(route);
                std::fs::write(&rotate_file, b"rotate").unwrap();
                return StatusCode::UNAUTHORIZED.into_response();
            }
            let expected_token = if auth_mode == AuthMode::Command {
                "Bearer synthetic-command-second"
            } else {
                "Bearer synthetic-r6-launch"
            };
            assert!(
                request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .is_some_and(|value| value == expected_token),
                "native CLI sent an unexpected provider credential"
            );
            if auth_mode == AuthMode::Command {
                assert!(
                    command_first.load(Ordering::SeqCst),
                    "native CLI skipped the initial rejected credential"
                );
                if rejected_routes.lock().unwrap().contains(&route) {
                    refreshed_route.store(true, Ordering::SeqCst);
                }
            }
            assert!(request.headers().get("ChatGPT-Account-ID").is_none());
            if is_models {
                return axum::Json(json!({"models": []})).into_response();
            }
            if auth_mode == AuthMode::Command {
                assert!(
                    refreshed_route.load(Ordering::SeqCst),
                    "native CLI did not retry a 401-rejected route with the refreshed credential"
                );
            }
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
                    if auth_mode == AuthMode::NativeApiKey {
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
    let provider_auth = match auth_mode {
        AuthMode::Command => format!(
            "auth={{command={},args=[]}}\n",
            toml::Value::String(auth_script.to_string_lossy().into_owned())
        ),
        AuthMode::StaticBearer | AuthMode::NativeApiKey => {
            "experimental_bearer_token=\"synthetic-r6-launch\"\n".to_owned()
        }
    };
    let openai_auth = auth_mode == AuthMode::NativeApiKey;
    let config = format!(
        "model=\"gpt-6-astra\"\nmodel_provider=\"custom\"\ncheck_for_update_on_startup=false\n[model_providers.custom]\nname=\"Synthetic R6 launch\"\nbase_url=\"http://{}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth={openai_auth}\n{provider_auth}",
        upstream.address,
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
    if auth_mode == AuthMode::Command {
        assert!(
            !auth_calls.exists(),
            "workbench preflight executed the provider auth command"
        );
    }
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
    if auth_mode == AuthMode::Command {
        assert!(first_command_token_seen.load(Ordering::SeqCst));
        assert!(refreshed_rejected_command_route.load(Ordering::SeqCst));
        assert!(auth_rotate.exists());
        assert!(
            std::fs::read_to_string(&auth_calls)
                .unwrap()
                .lines()
                .count()
                >= 2
        );
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
    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
    let before: toml::Value = toml::from_str(&config).unwrap();
    assert!(
        after["model_providers"] == before["model_providers"],
        "native provider configuration changed"
    );
    assert_eq!(after["model"], before["model"]);
    assert_eq!(after["model_provider"], before["model_provider"]);
    if openai_auth {
        assert!(
            std::fs::read_to_string(home.join("auth.json")).unwrap() == auth,
            "native authentication file changed"
        );
    }
    if matches!(auth_mode, AuthMode::NativeApiKey | AuthMode::Command) {
        let (journals, observed) = verify_recording_redaction(
            &directory.path().join(".codex-web/history"),
            if openai_auth {
                "R6_E_REDACTED"
            } else {
                "R6_E_HISTORY"
            },
        );
        assert!(journals >= 2, "both runs must record observations");
        assert!(
            observed,
            "redaction check must include the actual streamed reply"
        );
    }
}
