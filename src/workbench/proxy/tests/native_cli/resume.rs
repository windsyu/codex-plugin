use super::*;
use crate::workbench::test_native::NativeProbe;
use std::io::BufRead;
use std::path::Path;

const FIRST_PROMPT: &str = "请记住这个合成会话标记，不使用工具。";
const NEXT_PROMPT: &str = "这是恢复后的新输入，请延续先前会话。";
const FIRST_TEXT: &str = "R0_RESUME_HISTORY：这个合成会话已经建立。";
const NEXT_TEXT: &str = "R0_RESUME_NEW_INPUT：已在恢复的原会话中接收新输入。";

pub(super) fn session_evidence(home: &Path, text: &str) -> Option<uuid::Uuid> {
    for entry in walkdir::WalkDir::new(home.join("sessions"))
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "jsonl")
        })
        .take(8)
    {
        let file = std::fs::File::open(entry.path()).ok()?;
        let mut id = None;
        let mut complete = false;
        for line in std::io::BufReader::new(file.take(8 * 1024 * 1024))
            .lines()
            .map_while(Result::ok)
        {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value["type"] == "session_meta" {
                id = value["payload"]["id"]
                    .as_str()
                    .and_then(|id| uuid::Uuid::parse_str(id).ok());
            }
            if value["type"] == "event_msg"
                && matches!(
                    value["payload"]["type"].as_str(),
                    Some("task_complete" | "turn_complete")
                )
                && value["payload"]["last_agent_message"] == text
            {
                complete = true;
            }
        }
        if complete {
            return id;
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed official CLI and PTY; synthetic durable session and resume only"]
async fn installed_cli_explicit_resume_restores_history_uses_new_proxy_and_waits_for_new_input() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    let config = r#"model = "gpt-6-astra"
model_provider = "custom"
check_for_update_on_startup = false
[model_providers.custom]
name = "Synthetic resume provider"
base_url = "http://127.0.0.1:1/original/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "synthetic-resume-token"
"#;
    std::fs::write(home.join("config.toml"), config).unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let server_requests = requests.clone();
    let upstream = fixture(move |request| {
        let requests = server_requests.clone();
        async move {
            assert_eq!(request.headers().get(header::AUTHORIZATION).unwrap(), "Bearer synthetic-resume-token");
            let body: Value = serde_json::from_slice(&axum::body::to_bytes(request.into_body(), 2*1024*1024).await.unwrap()).unwrap();
            if let Some(title) = title_response(&body) { return title; }
            let index = requests.fetch_add(1, Ordering::SeqCst);
            assert!(index <= 1, "native resume must not replay a prior user submission");
            assert_eq!(body["model"], "gpt-6-astra");
            assert!(body["input"].to_string().contains(FIRST_PROMPT));
            if index == 1 {
                assert!(body["input"].to_string().contains(FIRST_TEXT), "resumed request lost native durable context");
                assert!(body["input"].to_string().contains(NEXT_PROMPT));
            }
            let text = if index == 0 { FIRST_TEXT } else { NEXT_TEXT };
            let response_id = format!("resp_resume_{index}");
            let bytes = [event(json!({"type":"response.created","response":{"id":response_id}})),
                event(json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","id":format!("msg_{index}"),"content":[{"type":"output_text","text":text}]}})),
                event(json!({"type":"response.completed","response":{"id":response_id}}))].concat();
            Response::builder().header(header::CONTENT_TYPE,"text/event-stream").body(Body::from(bytes)).unwrap()
        }
    }).await;
    let mut original_id = None;
    for phase in 0..2 {
        let (capture, receiver) = capture::channel(8 * 1024 * 1024, 256);
        let proxy = proxy_for(upstream.address, capture.clone()).await;
        let hub = LiveHub::new(LiveLimits::default());
        let _observer = Observer::start(
            receiver,
            hub.clone(),
            RedactionPolicy::new(vec!["synthetic-resume-token".into()]).unwrap(),
            DecoderLimits::default(),
        )
        .unwrap();
        let mut args = vec![
            "-c".into(),
            format!(
                "model_providers.custom.base_url={}",
                toml::Value::String(proxy.child_base_url())
            ),
        ];
        if let Some(id) = original_id {
            args.extend(["resume".into(), format!("{id}")]);
        }
        let mut native = NativeProbe::start(&home, &workspace, &args).unwrap();
        native.ready().await.unwrap();
        if phase == 1 {
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(400) {
                native.pump().await.unwrap();
            }
            assert_eq!(
                requests.load(Ordering::SeqCst),
                1,
                "resume alone sent a new model turn"
            );
            assert!(
                native.screen().contains("R0_RESUME_HISTORY"),
                "native resume did not restore prior screen history"
            );
        }
        native
            .submit(if phase == 0 {
                FIRST_PROMPT
            } else {
                NEXT_PROMPT
            })
            .await
            .unwrap();
        let expected = if phase == 0 { FIRST_TEXT } else { NEXT_TEXT };
        let outcome = timeout(Duration::from_secs(15), async {
            loop {
                native.pump().await.unwrap();
                if native.screen().contains(expected)
                    && let Some(id) = session_evidence(&home, expected)
                {
                    break id;
                }
            }
        })
        .await;
        assert!(
            outcome.is_ok(),
            "resume phase {phase} incomplete; requests={}, durable={:?}, synthetic screen={}",
            requests.load(Ordering::SeqCst),
            session_evidence(&home, expected),
            RedactionPolicy::new(vec![proxy.state.capability.clone()])
                .unwrap()
                .scrub(&native.screen())
                .as_str()
        );
        let id = outcome.unwrap();
        if phase == 0 {
            original_id = Some(id);
        } else {
            assert_eq!(Some(id), original_id);
        }
        // Native durable completion and the async observation projection have
        // independent consumers. Wait for the latter rather than assuming it
        // completed in the same scheduler tick as the rollout write.
        timeout(Duration::from_secs(2), async {
            while !hub
                .snapshot()
                .model_items()
                .iter()
                .any(|item| item.text == expected)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("resumed response must reach the live projection");
        native.quit().await.unwrap();
        assert_eq!(capture.stats().dropped_chunks, 0);
        // This proxy/capability is dropped before the next explicit CLI start.
    }
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    let before: toml::Value = toml::from_str(config).unwrap();
    let after: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
    assert_eq!(before["model_providers"], after["model_providers"]);
    eprintln!(
        "R0 explicit native resume: graceful exit, original thread/history, new per-process route, zero automatic turn replay, new input completed"
    );
}
