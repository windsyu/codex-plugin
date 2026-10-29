use super::*;

#[tokio::test]
async fn default_websocket_response_streams_text_without_redundant_response_ids() {
    let (sender, mut receiver) = channel();
    let mut decoder = decoder();
    let hub = crate::workbench::live::LiveHub::new(Default::default());
    let mut stream = sender.stream(Source {
        transport: Transport::WebSocket,
        ..source()
    });
    for value in [
        json!({"type":"response.created","response":{"id":"default_response"}}),
        delta("正在回答中文"),
    ] {
        stream.offer(ws(value), Instant::now());
        for event in receive(&mut receiver, &mut decoder).await {
            hub.apply(event);
        }
    }
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.model_items().len(), 1);
    assert_eq!(snapshot.model_items()[0].text, "正在回答中文");
    assert_eq!(
        snapshot.model_items()[0].key.response_id.as_deref(),
        Some("default_response")
    );
}

fn model() -> ModelDecoder {
    ModelDecoder::new(
        RedactionPolicy::new(vec![]).unwrap(),
        32,
        Transport::WebSocket,
    )
}
fn feed(model: &mut ModelDecoder, value: Value) -> Vec<Change> {
    let mut changes = Vec::new();
    model.message(
        Ok(crate::workbench::framing::WireMessage {
            kind: MessageKind::Text,
            bytes: serde_json::to_vec(&value).unwrap(),
        }),
        &mut |change| changes.push(change),
    );
    changes
}
fn created(id: &str) -> Value {
    json!({"type":"response.created","response":{"id":id}})
}
fn completed(id: &str) -> Value {
    json!({"type":"response.completed","response":{"id":id}})
}
fn has_text(changes: &[Change]) -> bool {
    changes
        .iter()
        .any(|c| matches!(c, Change::TextDelta { .. } | Change::TextReplace { .. }))
}

#[test]
fn websocket_default_lane_lifecycle_never_borrows_after_terminal_or_overlap() {
    let mut decoder = model();
    feed(&mut decoder, created("first"));
    assert!(has_text(&feed(&mut decoder, delta("第一"))));
    feed(&mut decoder, completed("first"));
    assert!(!has_text(&feed(&mut decoder, delta("迟到"))));
    feed(&mut decoder, created("second"));
    assert!(has_text(&feed(&mut decoder, delta("第二"))));
    feed(&mut decoder, created("overlap"));
    assert!(!has_text(&feed(&mut decoder, done("不明确"))));
    feed(&mut decoder, completed("overlap"));
    assert!(!has_text(&feed(&mut decoder, done("不能恢复猜测"))));
    let mut explicit = done("显式身份仍可读");
    explicit["response_id"] = json!("second");
    assert!(has_text(&feed(&mut decoder, explicit)));
}

#[test]
fn websocket_named_lanes_gaps_unknown_events_and_conflicting_ids_disable_inference() {
    for disruption in [
        "lane",
        "gap",
        "unknown",
        "conflict",
        "invalid_id",
        "invalid_json",
    ] {
        let mut decoder = model();
        feed(&mut decoder, created("first"));
        match disruption {
            "lane" => {
                feed(
                    &mut decoder,
                    json!({"type":"response.in_progress","stream_id":"named"}),
                );
            }
            "gap" => decoder.gap(&mut |_| {}),
            "unknown" => {
                feed(&mut decoder, json!({"type":"future.lifecycle"}));
            }
            "conflict" | "invalid_id" => {
                let mut event = done("显式内容");
                event["response_id"] = json!(if disruption == "conflict" {
                    "other"
                } else {
                    "bad/id"
                });
                feed(&mut decoder, event);
            }
            _ => decoder.message(
                Ok(crate::workbench::framing::WireMessage {
                    kind: MessageKind::Text,
                    bytes: b"{broken".to_vec(),
                }),
                &mut |_| {},
            ),
        }
        assert!(
            !has_text(&feed(&mut decoder, done("不明确"))),
            "{disruption}"
        );
        feed(&mut decoder, completed("first"));
        feed(&mut decoder, created("next"));
        assert!(
            !has_text(&feed(&mut decoder, done("仍不明确"))),
            "{disruption}"
        );
        let mut explicit = done("完整显式可恢复");
        explicit["response_id"] = json!("next");
        assert!(has_text(&feed(&mut decoder, explicit)), "{disruption}");
    }
}

#[test]
fn default_websocket_tool_items_share_the_proven_response_identity() {
    let mut decoder = model();
    feed(&mut decoder, created("tools"));
    let added = feed(
        &mut decoder,
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"tool","call_id":"call","name":"exec_command","arguments":""}}),
    );
    assert!(added.iter().any(|change| matches!(change, Change::Tool { update } if update.key.response_id.as_deref()==Some("tools"))));
    let done = feed(
        &mut decoder,
        json!({"type":"response.function_call_arguments.done","item_id":"tool","output_index":0,"arguments":"{\"cmd\":\"pwd\"}"}),
    );
    assert!(done.iter().any(|change| matches!(change, Change::Tool { update } if update.key.response_id.as_deref()==Some("tools"))));
}

fn request_body(prewarm: bool) -> Value {
    let mut body = json!({"type":"response.create","model":"fixture-model","client_metadata":{"x-codex-turn-metadata":json!({"request_kind":"turn","thread_source":"user","thread_id":"11111111-1111-4111-8111-111111111111","turn_id":"turn_1"}).to_string()}});
    if prewarm {
        body["generate"] = json!(false);
    }
    body
}
fn masked(value: Value) -> Bytes {
    let bytes = serde_json::to_vec(&value).unwrap();
    let mut frame = vec![0x81, 0xfe];
    frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    let mask = [1, 2, 3, 4];
    frame.extend(mask);
    frame.extend(bytes.iter().enumerate().map(|(i, byte)| byte ^ mask[i % 4]));
    Bytes::from(frame)
}

#[tokio::test]
async fn default_lane_creates_bind_by_complete_lifecycle_and_keep_prewarm_separate() {
    let (sender, mut receiver) = channel();
    let mut decoder = decoder();
    let hub = crate::workbench::live::LiveHub::new(Default::default());
    let response_source = Source {
        transport: Transport::WebSocket,
        ..source()
    };
    let mut request = sender.stream(Source {
        direction: Direction::Request,
        ..response_source.clone()
    });
    let mut response = sender.stream(response_source);
    for (prewarm, id) in [(true, "prewarm"), (false, "conversation")] {
        request.offer(masked(request_body(prewarm)), Instant::now());
        for event in receive(&mut receiver, &mut decoder).await {
            hub.apply(event);
        }
        response.offer(ws(created(id)), Instant::now());
        let events = receive(&mut receiver, &mut decoder).await;
        let info = events
            .iter()
            .find_map(|e| match &e.change {
                Change::Request { info } => Some(info),
                _ => None,
            })
            .unwrap();
        assert_eq!(info.response_id.as_deref(), Some(id));
        assert_eq!(info.client_request_index, Some(if prewarm { 1 } else { 2 }));
        assert_eq!(
            info.purpose,
            if prewarm {
                request::RequestPurpose::Auxiliary
            } else {
                request::RequestPurpose::Conversation
            }
        );
        assert_eq!(info.codex_turn_id.is_some(), !prewarm);
        for event in events {
            hub.apply(event);
        }
        if !prewarm {
            response.offer(ws(delta("中间正文")), Instant::now());
            for event in receive(&mut receiver, &mut decoder).await {
                hub.apply(event);
            }
            assert_eq!(
                hub.snapshot().model_items()[0]
                    .author
                    .requested_model
                    .as_deref(),
                Some("fixture-model")
            );
        }
        response.offer(ws(json!({"type":"response.completed","response":{"id":id,"usage":{"input_tokens":if prewarm {0} else {7}}}})), Instant::now());
        for event in receive(&mut receiver, &mut decoder).await {
            hub.apply(event);
        }
    }
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.requests.len(), 2);
    assert_eq!(snapshot.usage_summary.input_tokens.tokens, Some(7));
    assert_eq!(
        snapshot
            .responses
            .iter()
            .find(|r| r.response_id.as_deref() == Some("prewarm"))
            .unwrap()
            .usage
            .as_ref()
            .unwrap()
            .input_tokens,
        Some(0)
    );
    assert_eq!(
        snapshot
            .responses
            .iter()
            .find(|r| r.response_id.as_deref() == Some("conversation"))
            .unwrap()
            .usage
            .as_ref()
            .unwrap()
            .input_tokens,
        Some(7)
    );
}

#[tokio::test]
async fn websocket_request_overlap_named_lane_and_request_gap_never_bind_metadata() {
    for issue in ["overlap", "named", "gap", "unknown", "response_first"] {
        let (sender, mut receiver) = channel();
        let mut decoder = decoder();
        let source = Source {
            transport: Transport::WebSocket,
            ..source()
        };
        let mut request = sender.stream(Source {
            direction: Direction::Request,
            ..source.clone()
        });
        let mut response = sender.stream(source);
        if issue == "response_first" {
            response.offer(ws(created("arrived_before_create")), Instant::now());
            receive(&mut receiver, &mut decoder).await;
        }
        let mut body = request_body(false);
        if issue == "named" {
            body["stream_id"] = json!("lane");
        }
        if issue == "unknown" {
            body["type"] = json!("future.create");
        }
        request.offer(masked(body), Instant::now());
        let mut observation = receiver.recv().await.unwrap();
        if issue == "gap" {
            observation.sequence += 2;
        }
        decoder.push(&observation, |_| {});
        if issue == "overlap" {
            request.offer(masked(request_body(false)), Instant::now());
            receive(&mut receiver, &mut decoder).await;
        }
        response.offer(ws(created("unassigned")), Instant::now());
        let events = receive(&mut receiver, &mut decoder).await;
        assert!(
            !events.iter().any(
                |e| matches!(&e.change, Change::Request { info } if info.response_id.is_some())
            ),
            "{issue}"
        );
    }
}

#[tokio::test]
async fn websocket_model_state_eviction_cannot_pair_a_later_create_to_an_earlier_response() {
    let (sender, mut receiver) = channel();
    let mut decoder = decoder();
    let source = Source {
        transport: Transport::WebSocket,
        ..source()
    };
    let request_id = source.request_id;
    let mut request = sender.stream(Source {
        direction: Direction::Request,
        ..source.clone()
    });
    let mut response = sender.stream(source);
    request.offer(masked(request_body(false)), Instant::now());
    receive(&mut receiver, &mut decoder).await;
    // A capacity eviction can discard the response side before its first byte.
    decoder.streams.remove(&request_id);
    request.offer(masked(request_body(false)), Instant::now());
    receive(&mut receiver, &mut decoder).await;
    response.offer(ws(created("belongs_to_first_create")), Instant::now());
    let events = receive(&mut receiver, &mut decoder).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.change, Change::Request { info } if info.response_id.is_some()))
    );
}

#[test]
fn official_auxiliary_notifications_do_not_change_or_disable_response_identity() {
    let mut decoder = model();
    feed(&mut decoder, created("active"));
    for kind in [
        "codex.rate_limits",
        "codex.response.metadata",
        "response.metadata",
        "responsesapi.websocket_timing",
    ] {
        let changes = feed(
            &mut decoder,
            json!({"type":kind,"response":{"id":"not_a_lifecycle","usage":{"total_tokens":999}},"output_text":"NOT_MODEL_TEXT"}),
        );
        assert!(changes.is_empty());
        let text = feed(&mut decoder, delta("继续"));
        assert!(has_text(&text));
        assert!(text.iter().all(|change| !matches!(change, Change::TextDelta { key, .. } if key.response_id.as_deref() != Some("active"))));
    }
    assert!(
        feed(&mut decoder, json!({"type":"future.notification"}))
            .iter()
            .any(|change| matches!(
                change,
                Change::Diagnostic {
                    code: DiagnosticCode::UnknownEvent
                }
            ))
    );
    assert!(!has_text(&feed(&mut decoder, delta("不得归属"))));
}

#[test]
fn auxiliary_metadata_cannot_restore_a_named_or_gapped_websocket_lane() {
    for named in [true, false] {
        let mut decoder = model();
        feed(&mut decoder, created("active"));
        if named {
            feed(
                &mut decoder,
                json!({"type":"response.metadata","stream_id":"other"}),
            );
        } else {
            decoder.gap(&mut |_| {});
            feed(&mut decoder, json!({"type":"response.metadata"}));
        }
        assert!(!has_text(&feed(&mut decoder, delta("不得归属"))));
    }
}
