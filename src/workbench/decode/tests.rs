use super::*;
use crate::workbench::capture::{self, CaptureReceiver, CaptureSender};
use axum::body::Bytes;
use serde_json::json;

mod tools;

fn source() -> Source {
    Source {
        request_id: Uuid::new_v4(),
        direction: Direction::Response,
        transport: Transport::Http,
        content_type: "text/event-stream; charset=utf-8".into(),
        content_encoding: String::new(),
    }
}
fn decoder() -> Decoder {
    Decoder::new(
        DecoderLimits::default(),
        RedactionPolicy::new(vec!["fixture-secret-value".into()]).unwrap(),
    )
}
fn sse(value: Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}
fn delta(text: &str) -> Value {
    json!({"type":"response.output_text.delta","item_id":"msg_fixture","content_index":0,"output_index":0,"delta":text})
}
fn done(text: &str) -> Value {
    json!({"type":"response.output_text.done","item_id":"msg_fixture","content_index":0,"output_index":0,"text":text})
}
fn ws(value: Value) -> Bytes {
    let body = serde_json::to_vec(&value).unwrap();
    let mut frame = vec![0x81];
    if body.len() < 126 {
        frame.push(body.len() as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(body.len() as u16).to_be_bytes());
    }
    frame.extend(body);
    Bytes::from(frame)
}
async fn receive(receiver: &mut CaptureReceiver, decoder: &mut Decoder) -> Vec<Decoded> {
    let observation = receiver.recv().await.unwrap();
    let mut output = Vec::new();
    decoder.push(&observation, |event| output.push(event));
    output
}
fn serialized(events: &[Decoded]) -> String {
    serde_json::to_string(&events.iter().map(|event| &event.change).collect::<Vec<_>>()).unwrap()
}
fn channel() -> (CaptureSender, CaptureReceiver) {
    capture::channel(2 * 1024 * 1024, 128)
}

#[tokio::test]
async fn ended_transport_marks_only_unfinished_responses_incomplete() {
    for transport in [Transport::Http, Transport::WebSocket] {
        for interrupted in [false, true] {
            let (sender, mut receiver) = channel();
            let mut decoder = decoder();
            let hub = crate::workbench::live::LiveHub::new(Default::default());
            let mut stream = sender.stream(Source {
                transport,
                ..source()
            });
            let values = if transport == Transport::Http {
                vec![
                    json!({"type":"response.created","response":{"id":"pending"}}),
                    delta("partial text"),
                ]
            } else {
                vec![
                    json!({"type":"response.created","response":{"id":"finished"}}),
                    json!({"type":"response.created","response":{"id":"pending"}}),
                    json!({"type":"response.completed","response":{"id":"finished"}}),
                    json!({"type":"response.output_text.delta","response_id":"pending","item_id":"msg_pending","content_index":0,"delta":"partial text"}),
                ]
            };
            for value in values {
                stream.offer(
                    if transport == Transport::Http {
                        sse(value)
                    } else {
                        ws(value)
                    },
                    Instant::now(),
                );
                for event in receive(&mut receiver, &mut decoder).await {
                    hub.apply(event);
                }
            }
            if !interrupted {
                stream.finish();
            }
            drop(stream);
            for event in receive(&mut receiver, &mut decoder).await {
                hub.apply(event);
            }
            let snapshot = hub.snapshot();
            assert_eq!(
                snapshot
                    .responses
                    .iter()
                    .find(|r| r.response_id.as_deref() == Some("pending"))
                    .unwrap()
                    .status,
                ResponseStatus::Incomplete
            );
            assert_eq!(snapshot.model_items()[0].text, "partial text");
            if transport == Transport::WebSocket {
                assert_eq!(
                    snapshot
                        .responses
                        .iter()
                        .find(|r| r.response_id.as_deref() == Some("finished"))
                        .unwrap()
                        .status,
                    ResponseStatus::Completed
                );
            }
        }
    }
}

#[tokio::test]
async fn interleaved_ws_usage_requires_explicit_response_identity_and_documents_precede_final_state()
 {
    let (sender, mut receiver) = channel();
    let mut decoder = decoder();
    let mut stream = sender.stream(Source {
        transport: Transport::WebSocket,
        content_type: String::new(),
        ..source()
    });
    for value in [
        json!({"type":"response.created","response":{"id":"a"}}),
        json!({"type":"response.created","response":{"id":"b"}}),
        json!({"type":"response.completed","response":{"id":"b","usage":{"input_tokens":40},"output":[]}}),
        json!({"type":"response.completed","response":{"id":"a","usage":{"input_tokens":20},"output":[]}}),
        json!({"type":"response.completed","response":{"usage":{"input_tokens":99},"output":[]}}),
    ] {
        stream.offer(ws(value.clone()), Instant::now());
        let events = receive(&mut receiver, &mut decoder).await;
        if value["type"] == "response.completed" {
            let doc = events
                .iter()
                .position(|event| matches!(event.change, Change::Document { .. }))
                .unwrap();
            let final_state = events
                .iter()
                .position(|event| matches!(event.change, Change::Response { .. }))
                .unwrap();
            assert!(doc < final_state);
            let Change::Response {
                response_id, usage, ..
            } = &events[final_state].change
            else {
                unreachable!()
            };
            assert_eq!(response_id.as_deref(), value["response"]["id"].as_str());
            if response_id.is_none() {
                assert!(usage.is_none());
            } else {
                assert_eq!(
                    usage.as_ref().unwrap().input_tokens,
                    value["response"]["usage"]["input_tokens"].as_u64()
                );
            }
        }
    }
}

#[tokio::test]
async fn invalid_explicit_final_response_id_cannot_borrow_a_previous_usage_identity() {
    let (sender, mut receiver) = channel();
    let mut decoder = decoder();
    let mut stream = sender.stream(source());
    stream.offer(
        sse(json!({"type":"response.created","response":{"id":"valid-response"}})),
        Instant::now(),
    );
    receive(&mut receiver, &mut decoder).await;
    stream.offer(sse(json!({"type":"response.completed","response":{"id":"fixture-secret-value","usage":{"input_tokens":99},"output":[]}})), Instant::now());
    let events = receive(&mut receiver, &mut decoder).await;
    assert!(events.iter().any(|event| matches!(
        event.change,
        Change::Response {
            response_id: None,
            usage: None,
            ..
        }
    )));
    assert!(!serialized(&events).contains("fixture-secret-value"));
}

#[tokio::test]
async fn partial_text_is_safe_immediately_and_final_text_is_a_replacement() {
    let (sender, mut receiver) = channel();
    let source = source();
    let id = source.request_id;
    let mut stream = sender.stream(source);
    let mut decoder = decoder();
    let mut events = Vec::new();
    for value in [
        json!({"type":"response.created","response":{"id":"resp_fixture","encrypted_content":"opaque-must-not-appear"}}),
        delta("正文 fixture-sec"),
        delta("ret-value 正常"),
        done("正文 fixture-secret-value 正常"),
        json!({"type":"response.completed","response":{"id":"resp_fixture","output":[],"authorization":"never-expose","usage":{"input_tokens":3}}}),
    ] {
        stream.offer(sse(value), Instant::now());
        events.extend(receive(&mut receiver, &mut decoder).await);
    }
    let safe = serialized(&events);
    assert!(!safe.contains("fixture-secret-value"));
    assert!(!safe.contains("opaque-must-not-appear"));
    assert!(!safe.contains("never-expose"));
    assert!(safe.contains("[已脱敏]"));
    assert!(events.iter().all(|event| event.request_id == id));
    let mut text = String::new();
    let mut replaced = false;
    for event in events {
        match event.change {
            Change::TextDelta { key, text: delta } => {
                assert_eq!(key.request_id, id);
                text.push_str(delta.as_str());
            }
            Change::TextReplace { text: full, .. } => {
                text = full.as_str().to_owned();
                replaced = true;
            }
            _ => {}
        }
    }
    assert!(replaced);
    assert_eq!(text, "正文 [已脱敏] 正常");
}

#[tokio::test]
async fn malformed_json_and_unknown_fields_produce_safe_diagnostics_then_final_recovery() {
    let (sender, mut receiver) = channel();
    let mut stream = sender.stream(source());
    let mut decoder = decoder();
    let mut events = Vec::new();
    for bytes in [
        sse(delta("正常前缀 ")),
        Bytes::from_static(b"data: {broken-json-secret\n\n"),
        sse(delta("unsafe-suffix")),
        sse(json!({"type":"unrecognized-secret-event","raw":"hidden-payload"})),
        sse(json!({"type":"response.reasoning_text.delta","delta":"private-reasoning"})),
        sse(
            json!({"type":"response.output_item.done","item":{"type":"reasoning","encrypted_content":"ciphertext","summary":[]}}),
        ),
        sse(done("最终正文")),
    ] {
        stream.offer(bytes, Instant::now());
        events.extend(receive(&mut receiver, &mut decoder).await);
    }
    let safe = serialized(&events);
    for private in [
        "broken-json-secret",
        "unsafe-suffix",
        "unrecognized-secret-event",
        "hidden-payload",
        "private-reasoning",
        "ciphertext",
    ] {
        assert!(!safe.contains(private));
    }
    assert!(safe.contains("invalid_json"));
    assert!(safe.contains("unknown_event"));
    assert!(safe.contains("omitted_by_policy"));
    assert!(safe.contains("最终正文"));
}

#[tokio::test]
async fn capture_gap_never_concatenates_a_secret_suffix_into_public_text() {
    let (sender, mut receiver) = channel();
    let mut stream = sender.stream(source());
    let mut decoder = decoder();
    stream.offer(sse(delta("开始 fixture-sec")), Instant::now());
    let mut events = receive(&mut receiver, &mut decoder).await;
    stream.offer(
        Bytes::from(format!(
            "\n\n{}{}",
            String::from_utf8(sse(delta("ret-value")).to_vec()).unwrap(),
            String::from_utf8(sse(done("结束 fixture-secret-value")).to_vec()).unwrap()
        )),
        Instant::now(),
    );
    let mut observation = receiver.recv().await.unwrap();
    observation.sequence += 2;
    decoder.push(&observation, |event| events.push(event));
    let safe = serialized(&events);
    assert!(safe.contains("observation_gap"));
    assert!(!safe.contains("fixture-sec"));
    assert!(!safe.contains("ret-value"));
    assert!(safe.contains("结束 [已脱敏]"));
}

#[tokio::test]
async fn independent_requests_do_not_share_text_keys_or_completion_state() {
    let (sender, mut receiver) = channel();
    let mut first = sender.stream(source());
    let mut second = sender.stream(source());
    let mut decoder = decoder();
    let mut events = Vec::new();
    for (which, text) in [(0, "第一"), (1, "第二"), (0, "继续")] {
        let stream = if which == 0 { &mut first } else { &mut second };
        stream.offer(sse(delta(text)), Instant::now());
        events.extend(receive(&mut receiver, &mut decoder).await);
    }
    assert_eq!(events[0].request_id, events[2].request_id);
    assert_ne!(events[0].request_id, events[1].request_id);
    assert!(serialized(&events).contains("第一"));
    second.offer(
        sse(json!({"type":"response.completed","response":{"id":"resp_second"}})),
        Instant::now(),
    );
    let ended = receive(&mut receiver, &mut decoder).await;
    assert!(
        ended
            .iter()
            .all(|event| event.request_id == events[1].request_id)
    );
    assert!(!serialized(&ended).contains("turn"));
}

#[tokio::test]
async fn unsupported_content_compression_and_stream_resource_limits_are_explicit() {
    let (sender, mut receiver) = channel();
    let mut decoder = Decoder::new(
        DecoderLimits {
            streams: 2,
            frame_bytes: 1024,
            buffered_bytes: 64,
            text_streams: 2,
        },
        RedactionPolicy::new(vec![]).unwrap(),
    );
    let mut events = Vec::new();
    for encoding in ["gzip", "br", "zstd"] {
        let mut source = source();
        source.content_encoding = encoding.into();
        let mut stream = sender.stream(source);
        stream.offer(
            Bytes::from_static(b"compressed-secret-bytes"),
            Instant::now(),
        );
        events.extend(receive(&mut receiver, &mut decoder).await);
        stream.finish();
        events.extend(receive(&mut receiver, &mut decoder).await);
    }
    let mut streams = Vec::new();
    for _ in 0..4 {
        let mut stream = sender.stream(source());
        stream.offer(Bytes::from_static(b"data: "), Instant::now());
        events.extend(receive(&mut receiver, &mut decoder).await);
        streams.push(stream);
    }
    assert!(decoder.streams.len() <= 2);
    streams
        .last_mut()
        .unwrap()
        .offer(Bytes::from(vec![b'x'; 128]), Instant::now());
    events.extend(receive(&mut receiver, &mut decoder).await);
    assert!(decoder.buffered_bytes() <= 64);
    let safe = serialized(&events);
    assert!(safe.contains("unsupported_compression"));
    assert!(safe.contains("capacity"));
    assert!(!safe.contains("compressed-secret-bytes"));
}

#[tokio::test]
async fn websocket_text_needs_explicit_response_identity_and_preserves_multiple_responses() {
    let (sender, mut receiver) = channel();
    let mut source = source();
    source.transport = Transport::WebSocket;
    let mut stream = sender.stream(source);
    let mut decoder = decoder();
    let mut events = Vec::new();
    for response in [None, Some("resp_one"), Some("resp_two")] {
        let mut value = delta("中");
        if let Some(response) = response {
            value["response_id"] = json!(response);
        }
        stream.offer(ws(value), Instant::now());
        events.extend(receive(&mut receiver, &mut decoder).await);
    }
    assert!(serialized(&events).contains("missing_identity"));
    let keys: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.change {
            Change::TextDelta { key, .. } => Some(key),
            _ => None,
        })
        .collect();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
}

#[tokio::test]
async fn websocket_completion_flushes_only_its_response_while_connection_remains_open() {
    let (sender, mut receiver) = channel();
    let mut source = source();
    source.transport = Transport::WebSocket;
    let mut stream = sender.stream(source);
    let mut decoder = decoder();
    for response in ["resp_one", "resp_two"] {
        let mut value = delta("末尾e");
        value["response_id"] = json!(response);
        stream.offer(ws(value), Instant::now());
        receive(&mut receiver, &mut decoder).await;
    }
    stream.offer(
        ws(json!({"type":"response.completed","response":{"id":"resp_one"}})),
        Instant::now(),
    );
    let events = receive(&mut receiver, &mut decoder).await;
    let tails: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.change {
            Change::TextDelta { key, text } => Some((key.response_id.as_deref(), text.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(tails, vec![(Some("resp_one"), "e")]);
    stream.offer(
        ws(json!({"type":"response.completed","response":{"id":"resp_two"}})),
        Instant::now(),
    );
    let events = receive(&mut receiver, &mut decoder).await;
    assert!(events.iter().any(|event| matches!(&event.change, Change::TextDelta { key, text } if key.response_id.as_deref() == Some("resp_two") && text.as_str() == "e")));
}
