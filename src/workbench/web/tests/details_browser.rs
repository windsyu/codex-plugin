use super::*;
use crate::workbench::capture::{self, Direction, Source, Transport};
use crate::workbench::decode::DecoderLimits;
use crate::workbench::observe::Observer;
use portable_pty::CommandBuilder;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed Chrome; synthetic model capture and cat PTY only"]
async fn browser_reads_context_on_demand_without_losing_terminal_or_reading_state() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("R2-context-reading");
    std::fs::create_dir(&workspace).unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let mut command = CommandBuilder::new("/bin/cat");
    command.cwd(&workspace);
    command.env("CODEX_HOME", directory.path());
    let host =
        crate::workbench::terminal::TerminalHost::spawn(hub.epoch(), command, 24, 80).unwrap();
    let server = ReadingServer::bind_with_terminal(hub.clone(), host.handle())
        .await
        .unwrap();
    let (sender, receiver) = capture::channel(2 * 1024 * 1024, 64);
    let _observer = Observer::start(
        receiver,
        hub.clone(),
        RedactionPolicy::new(vec!["fixture-sensitive-value".into()]).unwrap(),
        DecoderLimits::default(),
    )
    .unwrap();
    let source = Source {
        request_id: Uuid::new_v4(),
        direction: Direction::Response,
        transport: Transport::Http,
        content_type: "text/event-stream".into(),
        content_encoding: String::new(),
    };
    let mut request = sender.stream(Source {
        direction: Direction::Request,
        content_type: "application/json".into(),
        ..source.clone()
    });
    let mut stream = sender.stream(source);
    let mut browser = tokio::process::Command::new("node");
    browser
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/web/e2e/r2-context-probe.cjs"
        ))
        .env("WORKBENCH_PROBE_URL", server.bootstrap_url())
        .env("DEBUG", "")
        .env("PWDEBUG", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("WORKBENCH_TEST_SCREENSHOT") {
        browser.env("WORKBENCH_PROBE_SCREENSHOT", path);
    }
    let mut browser = crate::workbench::probe_process::ProbeProcess::spawn(&mut browser).unwrap();
    let _liveness = browser.stdin.take();
    let mut lines = BufReader::new(browser.stdout.take().unwrap()).lines();
    let mut send =
        |value: Value| stream.offer(Bytes::from(format!("data: {value}\n\n")), Instant::now());
    let mut completed = false;
    tokio::time::timeout(Duration::from_secs(45), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let report:Value = serde_json::from_str(&line).unwrap(); println!("{report}");
            match report["stage"].as_str().unwrap() {
                "ready" => {
                    let mut input:Vec<_> = (0..36).map(|index| json!({"role":if index==0 {"developer"} else {"user"},"content":format!("R2_CONTEXT_HISTORY_{index}")})).collect();
                    input.push(json!({"role":"user","content":[{"type":"input_image","image_url":"PRIVATE_BASE64"}]}));
                    input.push(json!({"type":"reasoning","summary":[],"encrypted_content":"PRIVATE_REASONING"}));
                    let body = json!({"model":"gpt-fixture","instructions":"R2_SYSTEM_CONTEXT\n<svg onload=\"window.r2ContextInjected=1\">literal</svg>\nfixture-sensitive-value","client_metadata":{"x-codex-turn-metadata":json!({"request_kind":"turn","thread_source":"user","thread_id":Uuid::new_v4(),"turn_id":"context-turn"}).to_string()},"input":input,"tools":[{"type":"function","name":"read_fixture","parameters":{"type":"object","properties":{"password":{"type":"string","default":"PRIVATE_DEFAULT"},"path":{"type":"string"}}}}]});
                    request.offer(Bytes::from(body.to_string()), Instant::now()); request.finish();
                    send(json!({"type":"response.created","response":{"id":"context-response","model":"gpt-fixture"}}));
                    send(json!({"type":"response.output_text.delta","item_id":"context-model","content_index":0,"delta":"R2_CONTEXT_PARTIAL"}));
                }
                "reading" => {
                    send(json!({"type":"response.output_text.done","item_id":"context-model","content_index":0,"text":"R2_CONTEXT_FINAL"}));
                    send(json!({"type":"response.completed","response":{"id":"context-response","status":"completed","usage":{"input_tokens":100,"output_tokens":24,"total_tokens":124,"input_tokens_details":{"cached_tokens":40},"output_tokens_details":{"reasoning_tokens":6}},"output":[{"type":"message","id":"context-model","role":"assistant","content":[{"type":"output_text","text":"R2_CONTEXT_FINAL"}]}]}}));
                }
                "complete" => completed = true,
                _ => panic!("context Chrome probe failed at safe stage"),
            }
        }
        assert!(browser.wait().await.unwrap().success());
    }).await.unwrap();
    assert!(completed);
    assert_eq!(sender.stats().dropped_chunks, 0);
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.model_items().len(), 1);
    assert!(snapshot.user_messages().is_empty());
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("R2_SYSTEM_CONTEXT")
    );
}
