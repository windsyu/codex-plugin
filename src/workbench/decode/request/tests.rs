use super::*;
use crate::workbench::capture;
use axum::body::Bytes;
use serde_json::json;

fn body(source: &str) -> Value {
    json!({"model":"gpt-fixture","input":[{"role":"user","content":"PRIVATE_CONTEXT_MUST_NOT_PUBLISH"}],"client_metadata":{"thread_id":"11111111-1111-4111-8111-111111111111","turn_id":"turn_a","x-codex-turn-metadata":json!({"thread_id":"11111111-1111-4111-8111-111111111111","turn_id":"turn_a","thread_source":source,"request_kind":"turn","workspaces":{"PRIVATE_PATH":"PRIVATE_VALUE"}}).to_string()}})
}
fn policy() -> Arc<RedactionPolicy> {
    RedactionPolicy::new(vec!["fixture-secret".into()]).unwrap()
}

#[test]
fn purpose_uses_canonical_metadata_and_never_guesses_from_user_role_or_title_schema() {
    let user = metadata(&body("user"), None, &policy());
    assert_eq!(user.purpose, RequestPurpose::Conversation);
    let auxiliary = metadata(&body("system"), None, &policy());
    assert_eq!(auxiliary.purpose, RequestPurpose::Auxiliary);
    let mut unknown = body("future_source");
    unknown["text"] = json!({"format":{"schema":{"properties":{"title":{"type":"string"}}}}});
    assert_eq!(
        metadata(&unknown, None, &policy()).purpose,
        RequestPurpose::Unknown
    );
    unknown.as_object_mut().unwrap().remove("client_metadata");
    assert_eq!(
        metadata(&unknown, None, &policy()).purpose,
        RequestPurpose::Unknown
    );
    let safe = serde_json::to_string(&user).unwrap();
    assert!(!safe.contains("PRIVATE_"));
}

#[test]
fn installed_thread_title_source_is_auxiliary_without_inferring_from_schema() {
    // CLI 0.156.1 emits this canonical source for its independent title thread.
    let title = metadata(&body("thread_title"), None, &policy());
    assert_eq!(title.purpose, RequestPurpose::Auxiliary);
    assert_eq!(title.purpose_basis, PurposeBasis::CodexTurnMetadata);
    let mut conversation = body("user");
    conversation["text"] = json!({"format":{"schema":{"properties":{"title":{"type":"string"}}}}});
    assert_eq!(
        metadata(&conversation, None, &policy()).purpose,
        RequestPurpose::Conversation
    );
    let mut conflict = body("thread_title");
    conflict["client_metadata"]["thread_id"] = json!("different");
    assert_eq!(
        metadata(&conflict, None, &policy()).purpose,
        RequestPurpose::Unknown
    );
}

#[test]
fn conflicting_metadata_invalid_ids_and_secrets_cannot_create_a_chat_identity() {
    let mut value = body("user");
    value["client_metadata"]["thread_id"] = json!("different");
    value["model"] = json!("fixture-secret");
    let context = metadata(&value, None, &policy());
    assert_eq!(context.purpose_basis, PurposeBasis::ConflictingMetadata);
    assert!(context.codex_thread_id.is_none() && context.codex_turn_id.is_none());
    assert!(context.requested_model.is_none());
    value["client_metadata"] = json!({"x-codex-turn-metadata":"{\"thread_source\":\"user\",\"request_kind\":\"turn\",\"thread_id\":\"not-a-uuid\"}"});
    assert_eq!(
        metadata(&value, None, &policy()).purpose,
        RequestPurpose::Unknown
    );
    assert!(safe_name(&json!("<script>model</script>"), &policy()).is_none());
    let secret_id_policy =
        RedactionPolicy::new(vec!["11111111-1111-4111-8111-111111111111".into()]).unwrap();
    assert!(
        metadata(&body("user"), None, &secret_id_policy)
            .codex_thread_id
            .is_none()
    );
}

#[tokio::test]
async fn masked_websocket_creates_keep_independent_ordinals_without_assigning_responses() {
    let (sender, mut receiver) = capture::channel(4096, 8);
    let mut stream = sender.stream(Source {
        request_id: Uuid::new_v4(),
        direction: Direction::Request,
        transport: Transport::WebSocket,
        content_type: String::new(),
        content_encoding: String::new(),
    });
    let mut decoder = Decoder::new(DecoderLimits::default(), policy());
    let mut output = Vec::new();
    for source in ["user", "system"] {
        let mut value = body(source);
        value["type"] = json!("response.create");
        let bytes = serde_json::to_vec(&value).unwrap();
        let mask = [4, 8, 15, 16];
        let mut frame = vec![0x81, 0xfe];
        frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
        frame.extend_from_slice(&mask);
        frame.extend(
            bytes
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );
        for chunk in frame.chunks(31) {
            stream.offer(Bytes::copy_from_slice(chunk), Instant::now());
            decoder.push(&receiver.recv().await.unwrap(), |event| output.push(event));
        }
    }
    let infos: Vec<_> = output
        .iter()
        .filter_map(|event| match &event.change {
            Change::Request { info } => Some(info),
            _ => None,
        })
        .collect();
    assert_eq!(infos.len(), 2);
    assert_eq!(infos[0].client_request_index, Some(1));
    assert_eq!(infos[1].client_request_index, Some(2));
    assert_eq!(infos[0].purpose, RequestPurpose::Conversation);
    assert_eq!(infos[1].purpose, RequestPurpose::Auxiliary);
    let sources: Vec<_> = output
        .iter()
        .filter_map(|event| match &event.change {
            Change::Document { document } => Some(document.source.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        sources,
        [
            details::DetailSource::Request {
                client_request_index: Some(1)
            },
            details::DetailSource::Request {
                client_request_index: Some(2)
            }
        ]
    );
}

#[tokio::test]
async fn split_http_metadata_is_published_only_after_a_complete_body_and_bounds_hold() {
    let bytes = serde_json::to_vec(&body("user")).unwrap();
    for split in [0, 1, 37, bytes.len() - 1, bytes.len()] {
        let (sender, mut receiver) = capture::channel(4096, 8);
        let mut stream = sender.stream(Source {
            request_id: Uuid::new_v4(),
            direction: Direction::Request,
            transport: Transport::Http,
            content_type: "application/json".into(),
            content_encoding: String::new(),
        });
        let mut decoder = Decoder::new(DecoderLimits::default(), policy());
        let mut events = Vec::new();
        for chunk in [&bytes[..split], &bytes[split..]] {
            stream.offer(Bytes::copy_from_slice(chunk), Instant::now());
            let observation = receiver.recv().await.unwrap();
            decoder.push(&observation, |event| events.push(event));
            assert!(events.is_empty());
        }
        stream.finish();
        decoder.push(&receiver.recv().await.unwrap(), |event| events.push(event));
        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[0].change, Change::Document { document } if document.source == details::DetailSource::Request { client_request_index: None })
        );
        assert!(
            matches!(&events[1].change, Change::Request { info } if info.purpose == RequestPurpose::Conversation)
        );
        assert_eq!(decoder.buffered_bytes(), 0);
    }
}

#[tokio::test]
async fn request_limits_interruption_and_compression_never_publish_partial_metadata() {
    for (encoding, complete) in [("", true), ("", false), ("zstd", true)] {
        let (sender, mut receiver) = capture::channel(4096, 8);
        let mut stream = sender.stream(Source {
            request_id: Uuid::new_v4(),
            direction: Direction::Request,
            transport: Transport::Http,
            content_type: "application/json".into(),
            content_encoding: encoding.into(),
        });
        let mut decoder = Decoder::new(
            DecoderLimits {
                frame_bytes: 64,
                buffered_bytes: 128,
                ..DecoderLimits::default()
            },
            policy(),
        );
        stream.offer(
            Bytes::from(serde_json::to_vec(&body("user")).unwrap()),
            Instant::now(),
        );
        let mut events = Vec::new();
        decoder.push(&receiver.recv().await.unwrap(), |event| events.push(event));
        if complete {
            stream.finish();
        } else {
            drop(stream);
        }
        decoder.push(&receiver.recv().await.unwrap(), |event| events.push(event));
        assert!(events.iter().all(|event| !matches!(
            event.change,
            Change::Request { .. } | Change::Document { .. }
        )));
        assert!(!events.is_empty());
        assert!(decoder.buffered_bytes() <= 128);
    }
}
