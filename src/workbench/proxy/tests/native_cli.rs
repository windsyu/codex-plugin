//! Opt-in installed-CLI experiment. It uses only a synthetic loopback model,
//! temporary CODEX_HOME and synthetic workspace; no real model credentials.

use super::*;
use crate::workbench::decode::DecoderLimits;
use crate::workbench::live::{LiveHub, LiveLimits};
use crate::workbench::observe::Observer;
use crate::workbench::redaction::RedactionPolicy;
use crate::workbench::test_browser::BrowserProbe;
use crate::workbench::web::ReadingServer;
use portable_pty::{Child, CommandBuilder, PtySize, native_pty_system};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc;

const PROMPT: &str = "请用中文介绍这个合成测试，不运行工具。";

mod browser_flow;
mod chat_source;
mod contexts_browser;
mod history_browser;
mod interaction;
mod launcher;
mod launcher_home;
mod launcher_resume;
mod lifecycle_browser;
mod multi_run;
mod patch_browser;
mod patch_native;
mod resume;
mod rollout_tools;
mod steering_browser;
mod tools_browser;

struct NativeChild(Box<dyn Child + Send + Sync>);

impl Drop for NativeChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn event(value: Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}

fn title_response(body: &Value) -> Option<Response> {
    body.pointer("/text/format/schema/properties/title")?;
    let output = json!({"type":"message","role":"assistant","id":"msg_title","content":[{"type":"output_text","text":"{\"title\":\"合成接入测试\"}"}]});
    let bytes = [
        event(json!({"type":"response.created","response":{"id":"resp_title"}})),
        event(json!({"type":"response.output_item.done","output_index":0,"item":output})),
        event(json!({"type":"response.completed","response":{"id":"resp_title"}})),
    ]
    .concat();
    Some(
        Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(bytes))
            .unwrap(),
    )
}

// Reply to terminal capability/cursor queries, including split escape sequences.
fn terminal_queries(
    bytes: &[u8],
    terminal: &mut vt100::Parser,
    pending: &mut Vec<u8>,
    writer: &mut dyn Write,
) {
    for &byte in bytes {
        terminal.process(&[byte]);
        if byte == 0x1b {
            pending.clear();
            pending.push(byte);
        } else if !pending.is_empty() {
            pending.push(byte);
            if pending.len() >= 3 && (0x40..=0x7e).contains(&byte) {
                if pending.as_slice() == b"\x1b[6n" {
                    let (row, column) = terminal.screen().cursor_position();
                    write!(writer, "\x1b[{};{}R", row + 1, column + 1).unwrap();
                    writer.flush().unwrap();
                }
                let reply = match pending.as_slice() {
                    b"\x1b[c" | b"\x1b[0c" => Some(b"\x1b[?1;2c".as_slice()),
                    b"\x1b[>c" | b"\x1b[>0c" => Some(b"\x1b[>0;0;0c".as_slice()),
                    b"\x1b[?u" => Some(b"\x1b[?0u".as_slice()),
                    _ => None,
                };
                if let Some(reply) = reply {
                    writer.write_all(reply).unwrap();
                    writer.flush().unwrap();
                }
                pending.clear();
            } else if pending.len() > 32 {
                pending.clear();
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed official Codex CLI, system Chrome and a PTY"]
async fn installed_cli_uses_per_process_route_and_exposes_intermediate_sse_without_config_changes()
{
    verify_native_route(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed official CLI, system Chrome and PTY; synthetic configuration layers only"]
async fn installed_cli_route_override_preserves_profile_and_ignores_project_credentials() {
    verify_native_route(true, false).await;
    verify_native_route(true, true).await;
}

async fn verify_native_route(profile_layer: bool, project_layer: bool) {
    let directory = tempfile::tempdir().unwrap();
    let cli_home = directory.path().join("codex-home");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&cli_home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(
        workspace.join("README.md"),
        "# Synthetic R0 probe\nNo private project data.\n",
    )
    .unwrap();
    let config = r#"model = "gpt-6-astra"
model_provider = "custom"
check_for_update_on_startup = false

[model_providers.custom]
name = "Synthetic R0 provider"
base_url = "http://127.0.0.1:1/unreachable/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "synthetic-r0-token"
"#;
    std::fs::write(cli_home.join("config.toml"), config).unwrap();
    // The installed CLI strips model_providers from project-local config,
    // including trusted projects; credentials stay in the selected profile.
    let expected_token = if profile_layer {
        "synthetic-profile-token"
    } else {
        "synthetic-r0-token"
    };
    if profile_layer {
        std::fs::write(
            cli_home.join("r0.config.toml"),
            r#"[model_providers.custom]
name = "Synthetic profile provider"
base_url = "http://127.0.0.1:2/profile/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "synthetic-profile-token"
"#,
        )
        .unwrap();
    }
    let project_config = r#"model_reasoning_effort = "low"
[model_providers.custom]
name = "Synthetic project provider"
base_url = "http://127.0.0.1:3/project/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "synthetic-project-token"
"#;
    if project_layer {
        std::fs::create_dir(workspace.join(".codex")).unwrap();
        std::fs::write(workspace.join(".codex/config.toml"), project_config).unwrap();
    }
    let (request_seen_tx, mut request_seen_rx) = mpsc::channel(8);
    let (release_model_tx, release_model_rx) = oneshot::channel();
    let model_gate = Arc::new(Mutex::new(Some(release_model_rx)));
    let server_sent_completion = Arc::new(AtomicBool::new(false));
    let server_completion = server_sent_completion.clone();
    let request_count = Arc::new(AtomicUsize::new(0));
    let server_requests = request_count.clone();
    let upstream = fixture(move |request| {
        let request_seen_tx = request_seen_tx.clone();
        let model_gate = model_gate.clone();
        let server_completion = server_completion.clone();
        let server_requests = server_requests.clone();
        async move {
            if request.uri().path() != "/v1/responses" {
                return StatusCode::NOT_FOUND.into_response();
            }
            let auth_ok = request.headers().get(header::AUTHORIZATION)
                .is_some_and(|value| value == format!("Bearer {expected_token}").as_str());
            let bytes = axum::body::to_bytes(request.into_body(), 2 * 1024 * 1024).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            // Installed Codex also asks its model for a structured task title.
            // Preserve that native request as a separate synthetic response.
            if let Some(response) = title_response(&body) {
                assert!(auth_ok, "native auxiliary authentication was not preserved");
                return response;
            }
            server_requests.fetch_add(1, Ordering::SeqCst);
            let chinese_input_seen = body.to_string().contains(PROMPT);
            request_seen_tx.send((auth_ok, body["model"].as_str().map(str::to_owned), chinese_input_seen,
                !project_layer || body["reasoning"]["effort"] == "low")).await.unwrap();
            let Some(release_model_rx) = model_gate.lock().unwrap().take() else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let stream = async_stream::stream! {
                yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":"resp_r0_synthetic","status":"in_progress"}})));
                yield Ok(event(json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant","id":"msg_r0","content":[]}})));
                yield Ok(event(json!({"type":"response.output_text.delta","item_id":"msg_r0","output_index":0,"content_index":0,"delta":"R0_NATIVE_"})));
                // The model cannot finish until Chrome has actually rendered
                // the first delta. Event order alone would not prove streaming.
                release_model_rx.await.unwrap();
                yield Ok(event(json!({"type":"response.output_text.delta","item_id":"msg_r0","output_index":0,"content_index":0,"delta":"OK"})));
                yield Ok(event(json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","id":"msg_r0","content":[{"type":"output_text","text":"R0_NATIVE_OK","annotations":[]}]}})));
                server_completion.store(true, Ordering::SeqCst);
                yield Ok(event(json!({"type":"response.completed","response":{"id":"resp_r0_synthetic","status":"completed","output":[],"usage":{"input_tokens":5,"output_tokens":5,"total_tokens":10}}})));
            };
            Response::builder().header(header::CONTENT_TYPE, "text/event-stream").body(Body::from_stream(stream)).unwrap()
        }
    }).await;
    let (capture, observations) = capture::channel(8 * 1024 * 1024, 256);
    let proxy = proxy_for(upstream.address, capture.clone()).await;
    let hub = LiveHub::new(LiveLimits::default());
    let _observer = Observer::start(
        observations,
        hub.clone(),
        RedactionPolicy::new(vec!["synthetic-r0-token".into(), expected_token.into()]).unwrap(),
        DecoderLimits::default(),
    )
    .unwrap();
    let reading = ReadingServer::bind(hub).await.unwrap();
    let mut browser = BrowserProbe::start(
        &reading.bootstrap_url(),
        "R0_NATIVE_",
        "R0_NATIVE_OK",
        "native",
        None,
    )
    .unwrap();
    let ready = timeout(Duration::from_secs(15), browser.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        ready["stage"], "ready",
        "Chrome failed to initialize the reading probe"
    );
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 45,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let mut command = CommandBuilder::new(
        std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into()),
    );
    command.cwd(&workspace);
    command.env("CODEX_HOME", &cli_home);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    if profile_layer {
        command.arg("--profile");
        command.arg("r0");
    }
    command.arg("-c");
    command.arg(format!(
        "model_providers.custom.base_url={}",
        toml::Value::String(proxy.child_base_url())
    ));
    let child = NativeChild(pair.slave.spawn_command(command).unwrap());
    assert!(child.0.process_id().is_some());
    drop(pair.slave);
    let (output_tx, mut output_rx) = mpsc::channel(64);
    let reader_thread = std::thread::spawn(move || {
        let mut bytes = [0u8; 8192];
        while let Ok(size) = reader.read(&mut bytes) {
            if size == 0 || output_tx.blocking_send(bytes[..size].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut terminal = vt100::Parser::new(45, 120, 0);
    let mut queries = Vec::new();
    let mut initialized = false;
    let mut trust_accepted = false;
    let mut submitted = false;
    let mut ready_at = None;
    let mut pasted_at = None;
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    let mut saw_delta_before_completed = false;
    let mut release_model_tx = Some(release_model_tx);
    let mut completed = false;
    let mut browser_completed = false;
    let mut browser_failure = None;
    let mut saw_request = false;
    let outcome = timeout(Duration::from_secs(35), async {
        loop {
            tokio::select! {
                bytes = output_rx.recv() => {
                    let bytes = bytes.expect("native CLI exited before probe completion");
                    terminal_queries(&bytes, &mut terminal, &mut queries, writer.as_mut());
                    let screen = terminal.screen().contents();
                    if !initialized && (screen.contains("Choose your style") || screen.contains("Select a theme")) {
                        writer.write_all(b"\r").unwrap();
                        initialized = true;
                    } else if !trust_accepted && (screen.contains("Do you trust") || screen.contains("Do you want to work in this directory")) {
                        // Initial drawing precedes the input stream becoming
                        // ready; wait for the settled native prompt, then Enter.
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        writer.write_all(b"\r").unwrap();
                        writer.flush().unwrap();
                        trust_accepted = true;
                    }
                }
                seen = request_seen_rx.recv(), if !saw_request => {
                    let (auth_ok, model, chinese_input_seen, project_setting_applied) = seen.unwrap();
                    assert!(auth_ok, "native authentication was not preserved");
                    assert!(project_setting_applied, "permitted project setting did not reach the actual request");
                    assert!(chinese_input_seen, "Chinese input did not reach the model request");
                    assert_eq!(model.as_deref(), Some("gpt-6-astra"));
                    saw_request = true;
                }
                report = browser.next(), if !browser_completed => {
                    let report = report.unwrap();
                    match report["stage"].as_str().unwrap() {
                        "intermediate" => {
                            assert!(!server_sent_completion.load(Ordering::SeqCst));
                            saw_delta_before_completed = true;
                            release_model_tx.take().unwrap().send(()).unwrap();
                        }
                        "final" => completed = true,
                        "complete" => browser_completed = true,
                        _ => { browser_failure = Some(report); break; },
                    }
                }
                _ = tick.tick() => {},
            }
            let screen = terminal.screen().contents();
            if !submitted && trust_accepted && screen.contains("OpenAI Codex") && screen.contains('›') {
                let ready = ready_at.get_or_insert_with(Instant::now);
                if ready.elapsed() >= Duration::from_millis(250) && pasted_at.is_none() {
                    writer.write_all(format!("\x1b[200~{PROMPT}\x1b[201~").as_bytes()).unwrap();
                    writer.flush().unwrap();
                    pasted_at = Some(Instant::now());
                } else if pasted_at.is_some_and(|at| at.elapsed() >= Duration::from_millis(100)) && screen.contains(PROMPT) {
                    // Verify native composition before one Enter, instead of
                    // racing the startup reader with a fixed paste-to-key delay.
                    writer.write_all(b"\r").unwrap(); writer.flush().unwrap();
                    submitted = true;
                }
            }
            if saw_request && completed && browser_completed && terminal.screen().contents().contains("R0_NATIVE_OK") { break; }
        }
    }).await;
    let screen = RedactionPolicy::new(vec![proxy.state.capability.clone()])
        .unwrap()
        .scrub(&terminal.screen().contents());
    drop(child);
    drop(writer);
    drop(pair.master);
    drop(output_rx);
    reader_thread.join().unwrap();
    assert!(
        outcome.is_ok() && browser_failure.is_none(),
        "native probe failed (profile={profile_layer}, project={project_layer}, browser={browser_failure:?}); synthetic terminal screen:\n{}",
        screen.as_str()
    );
    assert!(submitted && saw_request && completed && saw_delta_before_completed);
    timeout(Duration::from_secs(5), browser.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(capture.stats().dropped_chunks, 0);
    assert_eq!(
        request_count.load(Ordering::SeqCst),
        1,
        "unexpected additional native model request"
    );
    // The CLI may record trust/preferences elsewhere in the temporary home;
    // the model route and original provider stanza must remain unchanged.
    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(cli_home.join("config.toml")).unwrap()).unwrap();
    let before: toml::Value = toml::from_str(config).unwrap();
    assert_eq!(after["model_providers"], before["model_providers"]);
    assert_eq!(after["model"], before["model"]);
    assert_eq!(after["model_provider"], before["model_provider"]);
    if profile_layer {
        let saved: toml::Value =
            toml::from_str(&std::fs::read_to_string(cli_home.join("r0.config.toml")).unwrap())
                .unwrap();
        assert_eq!(
            saved["model_providers"]["custom"]["base_url"].as_str(),
            Some("http://127.0.0.1:2/profile/v1")
        );
        assert_eq!(
            saved["model_providers"]["custom"]["experimental_bearer_token"].as_str(),
            Some("synthetic-profile-token")
        );
    }
    if project_layer {
        assert_eq!(
            std::fs::read_to_string(workspace.join(".codex/config.toml")).unwrap(),
            project_config
        );
    }
    eprintln!(
        "R0 synthetic native probe: ordinary CLI/PTY, Chinese submission, original model/auth, per-process route, Chrome DOM before upstream completion, final replacement and refresh, provider configuration unchanged"
    );
}
