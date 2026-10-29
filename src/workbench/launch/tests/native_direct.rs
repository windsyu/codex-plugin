//! Opt-in native compatibility probe: production launcher, isolated ordinary CLI,
//! original OpenAI provider, local WebSocket fixture, and native history reader.
use super::*;
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::{Router, routing::get};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn quote(value: &std::ffi::OsStr) -> String {
    format!("'{}'", value.to_str().unwrap().replace('\'', "'\\''"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed official CLI and loopback/PTY; synthetic credentials and local WS only"]
async fn installed_cli_native_direct_websocket_reply_and_native_history() {
    let binary = std::env::var_os("WORKBENCH_TEST_CODEX")
        .expect("set WORKBENCH_TEST_CODEX to an installed official CLI absolute path");
    assert!(Path::new(&binary).is_absolute());
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("codex-home");
    let user_home = directory.path().join("user-home");
    let cwd = directory.path().join("project");
    for path in [&home, &user_home, &cwd] {
        std::fs::create_dir(path).unwrap();
    }
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let app = Router::new().route("/v1/responses", get(move |headers: HeaderMap, ws: WebSocketUpgrade| {
        let observed = observed.clone();
        async move {
            if headers.get(header::AUTHORIZATION).is_none_or(|value| value != "Bearer synthetic-direct-key") {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            ws.on_upgrade(move |mut socket| async move {
                while let Some(Ok(Message::Text(text))) = socket.recv().await {
                    let body: Value = serde_json::from_str(&text).unwrap();
                    if body["type"] != "response.create" { continue; }
                    if body["generate"] == false {
                        let event = json!({"type":"response.completed","response":{"id":"direct_prewarm","status":"completed","output":[]}});
                        if socket.send(Message::Text(event.to_string().into())).await.is_err() { break; }
                        continue;
                    }
                    assert_eq!(body["model"], "gpt-6-astra");
                    assert!(body["input"].to_string().contains("DIRECT_NATIVE_PROMPT"));
                    observed.fetch_add(1, Ordering::SeqCst);
                    let item = json!({"type":"message","role":"assistant","id":"direct_message","content":[{"type":"output_text","text":"DIRECT_NATIVE_OK"}]});
                    for event in [
                        json!({"type":"response.created","response":{"id":"direct_response","model":"gpt-6-astra"}}),
                        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant","id":"direct_message","content":[]}}),
                        json!({"type":"response.output_text.delta","item_id":"direct_message","output_index":0,"content_index":0,"delta":"DIRECT_NATIVE_OK"}),
                        json!({"type":"response.output_item.done","output_index":0,"item":item}),
                        json!({"type":"response.completed","response":{"id":"direct_response","status":"completed","output":[item],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}}),
                    ] {
                        if socket.send(Message::Text(event.to_string().into())).await.is_err() { break; }
                    }
                }
            }).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = format!(
        r#"model="gpt-6-astra"
openai_base_url="http://{address}/v1"
check_for_update_on_startup=false
[tui]
screen_reader_detection_done=true
animations=false
[projects.{}]
trust_level="trusted"
"#,
        toml::Value::String(cwd.to_str().unwrap().into())
    );
    std::fs::write(home.join("config.toml"), &config).unwrap();
    std::fs::write(
        home.join("auth.json"),
        r#"{"auth_mode":"apikey","OPENAI_API_KEY":"synthetic-direct-key"}"#,
    )
    .unwrap();
    // Only the test wrapper changes HOME; production retains the user's home.
    let wrapper = directory.path().join("isolated-cli");
    script(
        &wrapper,
        &format!(
            "export HOME={}\nexport USERPROFILE={}\nunset CODEX_API_KEY CODEX_ACCESS_TOKEN OPENAI_API_KEY\nexec {} \"$@\"",
            quote(user_home.as_os_str()),
            quote(user_home.as_os_str()),
            quote(&binary),
        ),
    );
    let paths = WorkbenchPaths::from_user_home(&user_home).unwrap();
    let data = paths.history_dir();
    let options = LaunchOptions {
        home: home.clone(),
        cwd: cwd.clone(),
        workbench_paths: paths.clone(),
        executable: wrapper,
        native_profile: None,
        resume: None,
        entry_file: None,
        data_dir: None,
    };
    let runtime = WorkbenchRuntime::start(options).await.unwrap();
    assert!(runtime.proxy.is_none() && runtime._observer.is_none() && runtime._users.is_none());
    let handle = runtime.terminal();
    let mut attachment = handle.attach().await.unwrap();
    let connection_id = attachment.connection_id;
    let grant = handle.claim(connection_id).await.unwrap();
    let drain = tokio::spawn(async move { while attachment.events.recv().await.is_some() {} });
    let mut sequence = 0;
    let mut submitted = false;
    let mut enter_at = None;
    let mut theme = false;
    let mut trust = false;
    let mut last_screen = String::new();
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let snapshot = handle.attach().await.unwrap().snapshot;
            let mut parser = vt100::Parser::new(snapshot.rows, snapshot.cols, 0);
            parser.process(&snapshot.screen);
            parser.process(&snapshot.replay);
            let screen = parser.screen().contents();
            last_screen.clone_from(&screen);
            if screen.contains("DIRECT_NATIVE_OK") {
                break;
            }
            let input = if enter_at.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                enter_at = None;
                Some(b"\r".to_vec())
            } else if !theme && screen.contains("Select a theme") {
                theme = true;
                Some(b"\r".to_vec())
            } else if !trust && screen.contains("trust") && screen.contains("folder") {
                trust = true;
                Some(b"\r".to_vec())
            } else if !submitted && screen.contains('›') && screen.contains("Codex") {
                submitted = true;
                enter_at = Some(std::time::Instant::now() + Duration::from_millis(600));
                Some(b"\x1b[200~DIRECT_NATIVE_PROMPT\x1b[201~".to_vec())
            } else {
                None
            };
            if let Some(bytes) = input {
                sequence += 1;
                handle
                    .input(connection_id, grant.generation, sequence, bytes)
                    .await
                    .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "isolated synthetic CLI timed out: submitted={submitted}, theme={theme}, trust={trust}, model_requests={}, screen={last_screen:?}",
        requests.load(Ordering::SeqCst)
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    let snapshot = runtime.hub.snapshot();
    assert_eq!(snapshot.capture, "unavailable");
    assert!(
        snapshot.requests.is_empty()
            && snapshot.responses.is_empty()
            && snapshot.model_items().is_empty()
    );
    drain.abort();
    drop(runtime);
    server.abort();

    // Read the actual native rollout through the homepage's production library.
    let prepared =
        super::super::super::config::Prepared::load(&home, &cwd, &paths, None, Default::default())
            .unwrap();
    let settings = super::super::super::config::ConfigService::start(prepared).unwrap();
    let library =
        crate::history::library::HistoryLibrary::start(home.clone(), data, settings.handle())
            .unwrap();
    let library_handle = library.handle();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(page) = library_handle.list(Default::default(), false).await {
                for record in page["records"].as_array().unwrap() {
                    if record["kind"] == "native"
                        && let Some(id) = record["entryId"].as_str()
                        && let Ok(body) = library_handle.body(id.into(), Default::default()).await
                        && body.to_string().contains("DIRECT_NATIVE_OK")
                    {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("native reply must remain readable from the history library");
    let saved: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
    assert!(saved.get("model_provider").is_none());
    assert_eq!(
        saved["openai_base_url"].as_str().unwrap(),
        format!("http://{address}/v1")
    );
}
