use super::*;
use crate::workbench::decode::request::{PurposeBasis, RequestPurpose};
use crate::workbench::decode::tool::{ToolIdentity, ToolKind, ToolOperation, ToolUpdate};
use crate::workbench::rollout::{UserKey, UserSource};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[tokio::test]
async fn one_typed_contract_replays_messages_tools_notices_and_metadata_into_the_same_snapshot() {
    let hub = LiveHub::new(LiveLimits::default());
    let mut reader = hub.subscribe(hub.epoch(), 0).unwrap();
    let request_id = Uuid::new_v4();
    let thread = Uuid::new_v4();
    let policy = RedactionPolicy::new(vec![]).unwrap();
    let emit = |change| {
        hub.apply(Decoded {
            request_id,
            capture_seq: 12,
            received_at: Instant::now(),
            change,
        })
    };
    emit(Change::Request {
        info: RequestInfo {
            client_request_index: None,
            requested_model: Some(policy.scrub("requested-model")),
            codex_thread_id: Some(thread),
            codex_turn_id: Some("turn".into()),
            purpose: RequestPurpose::Conversation,
            purpose_basis: PurposeBasis::CodexTurnMetadata,
        },
    });
    hub.apply(event(request_id, "text", "中", false));
    let tool_key = TextKey {
        request_id,
        response_id: Some("resp_fixture".into()),
        wire_item_id: "call".into(),
        content_index: 0,
    };
    let tool = |operation, text| Change::Tool {
        update: ToolUpdate {
            key: tool_key.clone(),
            identity: ToolIdentity {
                tool_kind: ToolKind::Custom,
                call_id: Some("call".into()),
                name: Some("exec".into()),
                namespace: None,
                invalid_fields: false,
            },
            operation,
            text: policy.scrub(text),
            fingerprint: None,
        },
    };
    emit(tool(ToolOperation::Begin, "code"));
    emit(tool(ToolOperation::Append, " more"));
    hub.apply_user(UserRecord {
        key: UserKey {
            codex_thread_id: thread,
            codex_turn_id: "turn".into(),
            native_item_id: "native-user".into(),
        },
        role: "user",
        text: "用户提交".into(),
        revision: 1,
        truncated: false,
        omitted: true,
        source: UserSource {
            source_ref: Uuid::new_v4(),
            byte_offset: 42,
            ordinal: Some(1),
        },
    });
    hub.apply(event(request_id, "text", "文", false));
    // Metadata changes must replace the existing card with a new revision. No
    // request-order inference or separate frontend metadata mutation is needed.
    emit(Change::Response {
        response_id: Some("resp_fixture".into()),
        status: ResponseStatus::Completed,
        model: Some(policy.scrub("reported-model")),
        usage: None,
    });
    emit(Change::Diagnostic {
        code: DiagnosticCode::UnknownEvent,
    });
    let snapshot = hub.snapshot();
    let json = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(json["schemaVersion"], VIEW_SCHEMA_VERSION);
    assert!(json.get("tools").is_none() && json.get("userMessages").is_none());
    assert_eq!(json["items"].as_array().unwrap().len(), 4);
    let mut items = BTreeMap::<String, Value>::new();
    let mut patches = Vec::new();
    for sequence in 1..=snapshot.view_seq {
        let event: Value = serde_json::from_str(&reader.recv().await.unwrap().json).unwrap();
        assert_eq!(event["viewSeq"], sequence);
        match event["kind"].as_str().unwrap() {
            "item.replace" => {
                let item = event["item"].clone();
                let key = item["itemKey"].as_str().unwrap().to_owned();
                if let Some(previous) = items.get(&key) {
                    assert_eq!(item["kind"], previous["kind"]);
                    assert!(item["revision"].as_u64() > previous["revision"].as_u64());
                    assert_eq!(item["orderIndex"], previous["orderIndex"]);
                }
                items.insert(key, item);
            }
            "item.patch" => {
                let item = items
                    .get_mut(event["itemKey"].as_str().unwrap())
                    .expect("first event must create a typed card");
                assert_eq!(item["revision"], event["baseRevision"]);
                let text = if event["field"] == "text" {
                    assert_eq!(item["kind"], "message");
                    assert_eq!(item["author"]["role"], "assistant");
                    item["content"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|part| part["contentKey"] == event["contentKey"])
                        .unwrap()
                        .get_mut("text")
                        .unwrap()
                } else {
                    assert_eq!(item["kind"], "tool_call");
                    item.get_mut("arguments").unwrap()
                };
                *text = json!(format!(
                    "{}{}",
                    text.as_str().unwrap(),
                    event["append"].as_str().unwrap()
                ));
                item["revision"] = event["revision"].clone();
                item["truncated"] = event["truncated"].clone();
                patches.push(event["field"].as_str().unwrap().to_owned());
            }
            other => assert!(!["tool.replace", "tool.patch", "user.replace"].contains(&other)),
        }
    }
    assert_eq!(patches, ["arguments", "text"]);
    for item in json["items"].as_array().unwrap() {
        assert_eq!(items.get(item["itemKey"].as_str().unwrap()), Some(item));
    }
    let model = json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["author"]["role"] == "assistant")
        .unwrap();
    assert_eq!(model["content"][0]["text"], "中文");
    assert_eq!(model["author"]["requestedModel"], "requested-model");
    assert_eq!(model["author"]["reportedModels"], json!(["reported-model"]));
    assert_eq!(model["streamState"], "ended");
    assert_eq!(snapshot.tools()[0].execution, ExecutionState::Unobserved);
    let user = json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["author"]["role"] == "user")
        .unwrap();
    assert_eq!(user["evidence"][0]["origin"], "rollout");
    assert_eq!(user["evidence"][0]["source"]["byteOffset"], 42);
    assert!(user["evidence"][0].get("requestId").is_none());
    assert_eq!(user["completeness"], "omitted");
}

#[test]
fn failed_response_does_not_complete_unfinished_text_or_invent_tool_execution() {
    let hub = LiveHub::new(LiveLimits::default());
    let id = Uuid::new_v4();
    hub.apply(event(id, "unfinished", "partial", false));
    hub.apply(event(id, "finished", "complete", true));
    hub.apply(Decoded {
        request_id: id,
        capture_seq: 2,
        received_at: Instant::now(),
        change: Change::Response {
            response_id: Some("resp_fixture".into()),
            status: ResponseStatus::Failed,
            model: None,
            usage: None,
        },
    });
    let snapshot = serde_json::to_value(hub.snapshot()).unwrap();
    assert_eq!(snapshot["items"][0]["streamState"], "incomplete");
    assert_eq!(snapshot["items"][0]["completeness"], "partial");
    assert_eq!(snapshot["items"][1]["streamState"], "ended");
}
