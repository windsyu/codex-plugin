use super::*;

const TOOL_PROMPT: &str = "请运行一次只读命令，打印合成测试标记。";
const CANCEL_PROMPT: &str = "请开始第二轮持续输出，等待我取消。";
const FINAL_PROMPT: &str = "取消之后请回复最后一句。";

struct Cancelled(Arc<AtomicBool>);
impl Drop for Cancelled {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn completed(index: usize) -> Bytes {
    event(
        json!({"type":"response.completed","response":{"id":format!("resp_interaction_{index}")}}),
    )
}
pub(super) fn final_message(index: usize, text: &str) -> Response {
    let bytes = [event(json!({"type":"response.created","response":{"id":format!("resp_interaction_{index}")}})),
        event(json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","id":format!("msg_interaction_{index}"),"content":[{"type":"output_text","text":text}]}})), completed(index)].concat();
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(bytes))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed official Codex CLI and native PTY; isolated synthetic project only"]
async fn installed_cli_multiturn_tool_execution_escape_cancellation_and_next_input() {
    installed_interaction(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed official CLI and PTY; synthetic Code Mode tool only"]
async fn installed_cli_code_mode_tool_return_is_observed_before_native_cancel_and_recovery() {
    installed_interaction(true).await;
}

async fn installed_interaction(code_mode: bool) {
    let directory = tempfile::tempdir().unwrap();
    let cli_home = directory.path().join("codex-home");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&cli_home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(
        workspace.join("README.md"),
        "# Synthetic native interaction probe\n",
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
    let config = format!("{config}\n[features]\ncode_mode = {code_mode}\n");
    std::fs::write(cli_home.join("config.toml"), &config).unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let validated = Arc::new(Mutex::new(Vec::new()));
    let tool_result_seen = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let (server_requests, server_validated, server_tool, server_cancel) = (
        requests.clone(),
        validated.clone(),
        tool_result_seen.clone(),
        cancelled.clone(),
    );
    let upstream = fixture(move |request| {
        let (requests, validated, tool_result_seen, cancelled) = (server_requests.clone(), server_validated.clone(), server_tool.clone(), server_cancel.clone());
        async move {
            if request.uri().path() != "/v1/responses" { return StatusCode::NOT_FOUND.into_response(); }
            let auth_ok = request.headers().get(header::AUTHORIZATION).is_some_and(|value| value == "Bearer synthetic-r0-token");
            let bytes = axum::body::to_bytes(request.into_body(), 2 * 1024 * 1024).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            if let Some(response) = title_response(&body) { return response; }
            let index = requests.fetch_add(1, Ordering::SeqCst) + 1;
            let expected_prompt = match index { 1 | 2 => TOOL_PROMPT, 3 => CANCEL_PROMPT, _ => FINAL_PROMPT };
            validated.lock().unwrap().push(auth_ok && body["model"] == "gpt-6-astra" && body["input"].to_string().contains(expected_prompt));
            match index {
                1 => {
                    let arguments = json!({"cmd":"printf 'R0_TOOL_OK\\n'", "login":false, "max_output_tokens":100, "yield_time_ms":1000}).to_string();
                    let call = if code_mode {
                        json!({"type":"custom_tool_call","id":"ctc_r0","call_id":"call_r0","name":"exec","input":format!("text(await tools.exec_command({arguments}));")})
                    } else {
                        json!({"type":"function_call","id":"fc_r0","call_id":"call_r0","name":"exec_command","arguments":arguments})
                    };
                    let bytes = [event(json!({"type":"response.created","response":{"id":"resp_interaction_1"}})),
                        event(json!({"type":"response.output_item.done","output_index":0,"item":call})), completed(1)].concat();
                    Response::builder().header(header::CONTENT_TYPE, "text/event-stream").body(Body::from(bytes)).unwrap()
                }
                2 => {
                    let output_type = if code_mode { "custom_tool_call_output" } else { "function_call_output" };
                    let observed = body["input"].as_array().is_some_and(|items| items.iter().any(|item| item["type"] == output_type && item["call_id"] == "call_r0" && item["output"].to_string().contains("R0_TOOL_OK")));
                    if code_mode && let Some(item) = body["input"].as_array().and_then(|items| items.iter().find(|item| item["type"] == output_type && item["call_id"] == "call_r0")) {
                        assert!(item["output"].as_array().is_some_and(|items| items.iter().any(|part|
                            part["type"] == "input_text" && part["text"].as_str().is_some_and(|text|
                                serde_json::from_str::<Value>(text).is_ok_and(|result|
                                    result["exit_code"] == 0 && result["output"].as_str().is_some_and(|output| output.trim() == "R0_TOOL_OK"))))));
                    }
                    tool_result_seen.store(observed, Ordering::SeqCst);
                    final_message(2, "R0_TOOL_DONE")
                }
                3 => {
                    let stream = async_stream::stream! {
                        let _cancelled = Cancelled(cancelled);
                        yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":"resp_interaction_3"}})));
                        yield Ok(event(json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant","id":"msg_cancel","content":[]}})));
                        yield Ok(event(json!({"type":"response.output_text.delta","item_id":"msg_cancel","output_index":0,"content_index":0,"delta":"R0_CANCEL_STREAM"})));
                        std::future::pending::<()>().await;
                        yield Ok(completed(3));
                    };
                    Response::builder().header(header::CONTENT_TYPE, "text/event-stream").body(Body::from_stream(stream)).unwrap()
                }
                4 => final_message(4, "R0_RESUMED_OK"),
                _ => StatusCode::BAD_REQUEST.into_response(),
            }
        }
    }).await;
    let (capture, receiver) = capture::channel(8 * 1024 * 1024, 256);
    // Use the production no-deadline client, so the cancellation proof cannot be
    // satisfied by the shorter HTTP timeout used by synthetic unit tests.
    let proxy = ProxyServer::bind(
        Upstream::parse(&format!("http://{}/v1", upstream.address)).unwrap(),
        capture.clone(),
    )
    .await
    .unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let _observer = Observer::start(
        receiver,
        hub.clone(),
        RedactionPolicy::new(vec!["synthetic-r0-token".into()]).unwrap(),
        DecoderLimits::default(),
    )
    .unwrap();
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
    command.arg("-c");
    command.arg(format!(
        "model_providers.custom.base_url={}",
        toml::Value::String(proxy.child_base_url())
    ));
    let child = NativeChild(pair.slave.spawn_command(command).unwrap());
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
    let mut theme = false;
    let mut trusted = false;
    let mut phase = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    let outcome = timeout(Duration::from_secs(35), async {
        loop {
            tokio::select! {
                bytes = output_rx.recv() => {
                    let Some(bytes) = bytes else { return false; };
                    terminal_queries(&bytes, &mut terminal, &mut queries, writer.as_mut());
                }
                _ = tick.tick() => {}
            }
            let screen = terminal.screen().contents();
            if !theme && (screen.contains("Choose your style") || screen.contains("Select a theme"))
            {
                writer.write_all(b"\r").unwrap();
                writer.flush().unwrap();
                theme = true;
            } else if !trusted
                && (screen.contains("Do you trust")
                    || screen.contains("Do you want to work in this directory"))
            {
                tokio::time::sleep(Duration::from_millis(300)).await;
                writer.write_all(b"\r").unwrap();
                writer.flush().unwrap();
                trusted = true;
            } else {
                let prompt = match phase {
                    0 if trusted && screen.contains("OpenAI Codex") && screen.contains('›') => {
                        Some(TOOL_PROMPT)
                    }
                    1 if screen.contains("R0_TOOL_DONE")
                        && tool_result_seen.load(Ordering::SeqCst) =>
                    {
                        Some(CANCEL_PROMPT)
                    }
                    3 if cancelled.load(Ordering::SeqCst)
                        && screen.to_lowercase().contains("interrupted") =>
                    {
                        Some(FINAL_PROMPT)
                    }
                    _ => None,
                };
                if let Some(prompt) = prompt {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    writer
                        .write_all(format!("\x1b[200~{prompt}\x1b[201~").as_bytes())
                        .unwrap();
                    writer.flush().unwrap();
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    writer.write_all(b"\r").unwrap();
                    writer.flush().unwrap();
                    phase += 1;
                } else if phase == 2
                    && screen.contains("esc to interrupt")
                    && hub
                        .snapshot()
                        .model_items()
                        .iter()
                        .any(|item| item.text.contains("R0_CANCEL_STREAM"))
                {
                    writer.write_all(b"\x1b").unwrap();
                    writer.flush().unwrap();
                    phase = 3;
                } else if phase == 4 && screen.contains("R0_RESUMED_OK") {
                    return true;
                }
            }
        }
    })
    .await;
    let screen = RedactionPolicy::new(vec![proxy.state.capability.clone()])
        .unwrap()
        .scrub(&terminal.screen().contents());
    drop(child);
    drop(writer);
    drop(pair.master);
    drop(output_rx);
    reader_thread.join().unwrap();
    assert!(
        matches!(outcome, Ok(true)),
        "native interaction failed in phase {phase}; synthetic terminal:\n{}",
        screen.as_str()
    );
    assert_eq!(requests.load(Ordering::SeqCst), 4);
    assert!(validated.lock().unwrap().iter().all(|ok| *ok));
    assert!(tool_result_seen.load(Ordering::SeqCst));
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(capture.stats().dropped_chunks, 0);
    let before: toml::Value = toml::from_str(&config).unwrap();
    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(cli_home.join("config.toml")).unwrap()).unwrap();
    assert_eq!(after["model_providers"], before["model_providers"]);
    assert_eq!(after["model"], before["model"]);
    assert_eq!(after["model_provider"], before["model_provider"]);
    eprintln!(
        "R0 native interaction: Chinese multi-turn input, real read-only tool result, Esc disconnects unfinished upstream, next input succeeds; original provider configuration preserved"
    );
}
