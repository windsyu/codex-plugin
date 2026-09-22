use super::*;
use crate::workbench::capture::{self, Direction, Source, Transport};
use crate::workbench::decode::DecoderLimits;
use crate::workbench::observe::Observer;
use crate::workbench::test_browser::BrowserProbe;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed Chrome; synthetic capture and cat PTY, no model or official CLI"]
async fn workbench_keeps_dom_keys_when_final_items_acquire_wire_ids() {
    use portable_pty::CommandBuilder;
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let directory = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let mut command = CommandBuilder::new("/bin/cat");
    command.cwd(directory.path());
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
        RedactionPolicy::new(vec![]).unwrap(),
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
            "/web/e2e/r2-identity-probe.cjs"
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
    let mut complete = false;
    tokio::time::timeout(Duration::from_secs(40), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let status: Value = serde_json::from_str(&line).unwrap();
            println!("{status}");
            match status["stage"].as_str().unwrap() {
                "ready" => {
                    let body = json!({"model":"gpt-5.5","client_metadata":{"x-codex-turn-metadata":json!({"request_kind":"turn","thread_source":"user","thread_id":Uuid::new_v4(),"turn_id":"synthetic-turn"}).to_string()},"input":[]});
                    request.offer(Bytes::from(body.to_string()), Instant::now()); request.finish();
                    send(json!({"type":"response.created","response":{"id":"identity-response"}}));
                    send(json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"R2_IDENTITY_PARTIAL"}));
                    send(json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"identity-call","name":"exec_command","arguments":""}}));
                    send(json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"cmd\":\"echo partial\"}"}));
                }
                "partial" => {
                    send(json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":" SNAPSHOT_RECOVERY"}));
                    send(json!({"type":"response.created","response":{"id":"identity-response","model":"fixture-reported"}}));
                }
                "recovered" => {
                    let model = json!({"type":"message","role":"assistant","id":"identity-model","content":[{"type":"output_text","text":"R2_IDENTITY_FINAL"}]});
                    let tool = json!({"type":"function_call","id":"identity-tool","call_id":"identity-call","name":"exec_command","arguments":"{\"cmd\":\"echo final\"}"});
                    send(json!({"type":"response.output_item.done","output_index":0,"item":model}));
                    send(json!({"type":"response.output_item.done","output_index":1,"item":tool}));
                    send(json!({"type":"response.completed","response":{"id":"identity-response","output":[model,tool]}}));
                }
                "complete" => { complete = true; }
                _ => panic!("identity browser check failed at safe stage"),
            }
        }
        assert!(browser.wait().await.unwrap().success());
    }).await.unwrap();
    assert!(complete);
    assert_eq!(hub.snapshot().model_items().len(), 1);
    assert_eq!(hub.snapshot().tools().len(), 1);
    assert!(hub.snapshot().diagnostics.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed system Chrome; no browser download or real data"]
async fn system_chrome_observes_safe_midstream_content_replacement_and_refresh() {
    let hub = LiveHub::new(LiveLimits::default());
    let (sender, receiver) = capture::channel(2 * 1024 * 1024, 64);
    let _observer = Observer::start(
        receiver,
        hub.clone(),
        RedactionPolicy::new(vec!["fixture-sensitive-value".into()]).unwrap(),
        DecoderLimits::default(),
    )
    .unwrap();
    let server = ReadingServer::bind(hub.clone()).await.unwrap();
    let mut stream = sender.stream(Source {
        request_id: Uuid::new_v4(),
        direction: Direction::Response,
        transport: Transport::Http,
        content_type: "text/event-stream".into(),
        content_encoding: String::new(),
    });
    let mut send =
        |value: Value| stream.offer(Bytes::from(format!("data: {value}\n\n")), Instant::now());
    let final_text = "R0_FINAL_OK\n<img src=x onerror=\"window.r0Injected=1\">\n[已脱敏]";
    let screenshot = std::env::var_os("WORKBENCH_TEST_SCREENSHOT").map(std::path::PathBuf::from);
    let mut browser = BrowserProbe::start(
        &server.bootstrap_url(),
        "R0_PARTIAL_",
        final_text,
        "reading",
        screenshot.as_deref(),
    )
    .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(40), async {
        let mut intermediate = false;
        loop {
            let report = browser.next().await.unwrap();
            match report["stage"].as_str().unwrap() {
                "ready" => {
                    send(json!({"type":"response.created","response":{"id":"resp_one"}}));
                    send(json!({"type":"response.output_text.delta","item_id":"msg_one","content_index":0,"delta":"R0_PARTIAL_fixture-sens"}));
                }
                "intermediate" => {
                    assert!(hub.snapshot().responses.iter().all(|response| response.status == crate::workbench::decode::ResponseStatus::Receiving));
                    intermediate = true;
                    send(json!({"type":"response.output_text.delta","item_id":"msg_one","content_index":0,"delta":"itive-value"}));
                    send(json!({"type":"response.output_text.done","item_id":"msg_one","content_index":0,"text":"R0_FINAL_OK\n<img src=x onerror=\"window.r0Injected=1\">\nfixture-sensitive-value"}));
                    send(json!({"type":"response.completed","response":{"id":"resp_one","output":[]}}));
                }
                "final" => assert!(intermediate),
                "complete" => { eprintln!("R0 Chrome safe reading probe: {report}"); break; }
                _ => panic!("Chrome probe failed: {report}"),
            }
        }
        browser.wait().await.unwrap();
    }).await;
    assert!(outcome.is_ok(), "Chrome reading probe deadline");
    assert_eq!(sender.stats().dropped_chunks, 0);
}
