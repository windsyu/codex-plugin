use super::*;
use crate::workbench::capture::{self, CaptureSender, CaptureStream, Direction, Source, Transport};
use crate::workbench::decode::DecoderLimits;
use crate::workbench::live::ExecutionState;
use crate::workbench::observe::Observer;
use portable_pty::CommandBuilder;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

fn source(id: Uuid, direction: Direction, transport: Transport) -> Source {
    Source {
        request_id: id,
        direction,
        transport,
        content_type: if direction == Direction::Request {
            "application/json".into()
        } else {
            "text/event-stream".into()
        },
        content_encoding: String::new(),
    }
}

fn metadata(thread: Uuid, turn: &str, kind: &str) -> Value {
    json!({"x-codex-turn-metadata": json!({"thread_id":thread,"turn_id":turn,"thread_source":"user","request_kind":kind}).to_string()})
}

fn http_request(sender: &CaptureSender, id: Uuid, body: Value) {
    let mut stream = sender.stream(source(id, Direction::Request, Transport::Http));
    stream.offer(Bytes::from(body.to_string()), Instant::now());
    stream.finish();
}

fn sse(stream: &mut CaptureStream, value: Value) {
    stream.offer(Bytes::from(format!("data: {value}\n\n")), Instant::now());
}

// Exercise the actual masked client frame decoder, including split frame headers.
fn ws(stream: &mut CaptureStream, value: Value, masked: bool) {
    let bytes = serde_json::to_vec(&value).unwrap();
    let mut frame = vec![0x81];
    let bit = if masked { 0x80 } else { 0 };
    if bytes.len() < 126 {
        frame.push(bit | bytes.len() as u8);
    } else {
        frame.push(bit | 126);
        frame.extend_from_slice(&u16::try_from(bytes.len()).unwrap().to_be_bytes());
    }
    let mask = [4, 8, 15, 16];
    if masked {
        frame.extend_from_slice(&mask);
    }
    frame.extend(bytes.iter().enumerate().map(|(index, byte)| {
        if masked {
            byte ^ mask[index % 4]
        } else {
            *byte
        }
    }));
    for chunk in frame.chunks(if masked { 31 } else { frame.len() }) {
        stream.offer(Bytes::copy_from_slice(chunk), Instant::now());
    }
}

fn call(id: &str, text: &str) -> Value {
    json!({"type":"custom_tool_call","id":format!("item-{id}"),"call_id":id,"name":"exec","input":text})
}

fn message(id: &str, text: &str) -> Value {
    json!({"type":"message","role":"assistant","id":id,"content":[{"type":"output_text","text":text}]})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed Chrome; synthetic HTTP/WS capture and cat PTY only"]
async fn browser_keeps_reverse_tool_results_and_websocket_contexts_in_their_proven_scope() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("R2-concurrent-requests");
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
    let (sender, receiver) = capture::channel(2 * 1024 * 1024, 512);
    let _observer = Observer::start(
        receiver,
        hub.clone(),
        RedactionPolicy::new(vec!["fixture-sensitive-value".into()]).unwrap(),
        DecoderLimits::default(),
    )
    .unwrap();
    let thread = Uuid::new_v4();
    let http_id = Uuid::new_v4();
    let next_id = Uuid::new_v4();
    let ws_id = Uuid::new_v4();
    let mut http = sender.stream(source(http_id, Direction::Response, Transport::Http));
    let mut follow = sender.stream(source(next_id, Direction::Response, Transport::Http));
    let mut creates = sender.stream(source(ws_id, Direction::Request, Transport::WebSocket));
    let mut responses = sender.stream(source(ws_id, Direction::Response, Transport::WebSocket));
    let introduction = (1..=35)
        .map(|n| {
            format!("R2_PARALLEL_INTRO {n}：先阅读这一段，工具结果更新时保留当前阅读位置。\n\n")
        })
        .collect::<String>();
    let first = call("http-first", "text('R2_HTTP_FIRST_FINAL');");
    let second = call("http-second", "text('R2_HTTP_SECOND_FINAL');");
    let ws_first = call("ws-shared-call", "text('R2_WS_ALPHA_FINAL');");
    let ws_second = call("ws-shared-call", "text('R2_WS_BETA_FINAL');");

    let mut browser = tokio::process::Command::new("node");
    browser
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/web/e2e/r2-concurrency-probe.cjs"
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
    let mut complete = false;
    tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let report: Value = serde_json::from_str(&line).unwrap();
            println!("{report}");
            match report["stage"].as_str().unwrap() {
                "ready" => {
                    http_request(&sender, http_id, json!({"model":"fixture-http","client_metadata":metadata(thread,"parallel-turn","turn"),"input":[{"role":"user","content":"R2_HISTORY_IS_CONTEXT_ONLY"}],"tools":[{"type":"custom","name":"exec","format":{"type":"text"}}]}));
                    sse(&mut http, json!({"type":"response.created","response":{"id":"http-response"}}));
                    sse(&mut http, json!({"type":"response.output_text.done","output_index":0,"item_id":"http-intro","content_index":0,"text":introduction}));
                    for (index, id, text) in [(1,"http-first","R2_HTTP_FIRST_PARTIAL"),(2,"http-second","R2_HTTP_SECOND_PARTIAL")] {
                        let mut item = call(id, "");
                        item.as_object_mut().unwrap().remove("id");
                        sse(&mut http, json!({"type":"response.output_item.added","output_index":index,"item":item}));
                        sse(&mut http, json!({"type":"response.custom_tool_call_input.delta","output_index":index,"delta":text}));
                    }
                }
                "http-partial" => {
                    for (index, item) in [(1,&first),(2,&second)] {
                        sse(&mut http, json!({"type":"response.output_item.done","output_index":index,"item":item}));
                    }
                    let final_response = json!({"type":"response.completed","response":{"id":"http-response","output":[message("http-intro",&introduction),first,second]}});
                    sse(&mut http, final_response.clone());
                    sse(&mut http, final_response);
                    http.finish();
                    let first_output = json!({"type":"custom_tool_call_output","call_id":"http-first","output":"R2_FIRST_RESULT <img src=x onerror=\"window.concurrentInjected=1\"> fixture-sensitive-value"});
                    let second_output = json!({"type":"custom_tool_call_output","call_id":"http-second","output":"R2_SECOND_RESULT"});
                    http_request(&sender, next_id, json!({"model":"fixture-http","client_metadata":metadata(thread,"parallel-turn","turn"),"input":[first,second,second_output,first_output,second_output,first_output]}));
                    sse(&mut follow, json!({"type":"response.created","response":{"id":"http-follow"}}));
                    sse(&mut follow, json!({"type":"response.completed","response":{"id":"http-follow","output":[message("http-follow-message","R2_HTTP_FOLLOWING_REPLY")]}}));
                    follow.finish();
                }
                "http-reviewed" => {
                    for (model, turn, kind, previous, input) in [
                        ("fixture-create-one","ws-turn-one","turn","previous-fixture-response","R2_WS_CONTEXT_ONE"),
                        ("fixture-create-two","ws-turn-two","compaction","another-previous-response","R2_WS_CONTEXT_TWO"),
                    ] {
                        ws(&mut creates, json!({"type":"response.create","model":model,"previous_response_id":previous,"client_metadata":metadata(thread,turn,kind),"input":[{"role":"user","content":input}]}), true);
                    }
                    // Server arrival order deliberately differs from create order.
                    for id in ["ws-beta","ws-alpha"] {
                        ws(&mut responses, json!({"type":"response.created","response":{"id":id,"model":format!("reported-{id}")}}), false);
                        ws(&mut responses, json!({"type":"response.output_item.added","response_id":id,"output_index":0,"item":call("ws-shared-call","")}), false);
                    }
                    for (id, text) in [("ws-alpha","R2_WS_ALPHA_PARTIAL"),("ws-beta","R2_WS_BETA_PARTIAL")] {
                        ws(&mut responses, json!({"type":"response.custom_tool_call_input.delta","response_id":id,"output_index":0,"delta":text}), false);
                        ws(&mut responses, json!({"type":"response.output_text.delta","response_id":id,"output_index":1,"item_id":"shared-model-item","content_index":0,"delta":format!("{text}_MODEL")}), false);
                    }
                    ws(&mut responses, json!({"type":"response.custom_tool_call_input.delta","output_index":0,"delta":"R2_UNSCOPED_MUST_NOT_APPEAR"}), false);
                }
                "ws-partial" => {
                    ws(&mut responses, json!({"type":"response.completed","response":{"id":"ws-alpha","usage":{"input_tokens":21,"output_tokens":5,"total_tokens":26},"output":[ws_first,message("shared-model-item","R2_WS_ALPHA_MODEL_FINAL")]}}), false);
                    ws(&mut creates, json!({"type":"response.create","model":"fixture-create-three","previous_response_id":"ws-alpha","client_metadata":metadata(thread,"ws-turn-three","turn"),"input":[ws_first,{"type":"custom_tool_call_output","call_id":"ws-shared-call","output":"R2_WS_UNASSIGNED_RESULT"},{"type":"input_image","image_url":"PRIVATE_WS_BASE64"},{"type":"reasoning","encrypted_content":"PRIVATE_WS_REASONING"}]}), true);
                }
                "ws-half" => {
                    let final_response = json!({"type":"response.completed","response":{"id":"ws-beta","usage":{"input_tokens":34,"output_tokens":8,"total_tokens":42},"output":[ws_second,message("shared-model-item","R2_WS_BETA_MODEL_FINAL")]}});
                    ws(&mut responses, final_response.clone(), false);
                    ws(&mut responses, final_response, false);
                    ws(&mut responses, json!({"type":"response.completed","response":{"usage":{"input_tokens":99,"total_tokens":99},"output":[]}}), false);
                    creates.finish();
                    responses.finish();
                }
                "complete" => complete = true,
                _ => panic!("concurrent Chrome probe failed at safe stage"),
            }
        }
        assert!(browser.wait().await.unwrap().success());
    }).await.unwrap();
    assert!(complete);
    assert_eq!(sender.stats().dropped_chunks, 0);
    let snapshot = hub.snapshot();
    assert!(snapshot.user_messages().is_empty());
    assert_eq!(snapshot.model_items().len(), 4);
    assert_eq!(snapshot.tools().len(), 4);
    assert_eq!(snapshot.requests.len(), 5);
    for tool in snapshot.tools() {
        if tool.key.request_id == ws_id {
            assert!(tool.result.is_none());
            assert_eq!(tool.execution, ExecutionState::Unobserved);
        } else {
            assert!(tool.result.is_some());
            assert_eq!(tool.execution, ExecutionState::ResultObserved);
        }
    }
    let safe = serde_json::to_string(&snapshot).unwrap();
    for forbidden in [
        "fixture-sensitive-value",
        "R2_UNSCOPED_MUST_NOT_APPEAR",
        "PRIVATE_WS_BASE64",
        "PRIVATE_WS_REASONING",
    ] {
        assert!(!safe.contains(forbidden));
    }
}
