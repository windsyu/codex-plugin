use super::*;
use crate::workbench::decode::request::{PurposeBasis, RequestPurpose};
use crate::workbench::decode::tool::{ToolKind, fingerprint};
use crate::workbench::decode::tool_context::{ToolDefinition, extract};
use crate::workbench::redaction::RedactionPolicy;
use crate::workbench::rollout::UserKey;
use serde_json::json;

fn emit(hub: &LiveHub, request_id: Uuid, change: Change) {
    hub.apply(Decoded {
        request_id,
        capture_seq: 1,
        received_at: Instant::now(),
        change,
    });
}

fn request(
    hub: &LiveHub,
    id: Uuid,
    index: Option<u64>,
    response: Option<&str>,
    thread: Uuid,
    turn: &str,
) {
    emit(
        hub,
        id,
        Change::Request {
            info: RequestInfo {
                client_request_index: index,
                response_id: response.map(str::to_owned),
                requested_model: None,
                codex_thread_id: Some(thread),
                codex_turn_id: Some(turn.into()),
                purpose: RequestPurpose::Conversation,
                purpose_basis: PurposeBasis::CodexTurnMetadata,
            },
        },
    );
}

fn context(
    hub: &LiveHub,
    id: Uuid,
    index: Option<u64>,
    name: &str,
    kind: ToolKind,
    category: ToolCategory,
) {
    emit(
        hub,
        id,
        Change::ToolContext {
            context: ToolContext {
                client_request_index: index,
                definitions: vec![ToolDefinition {
                    name: name.into(),
                    namespace: None,
                    tool_kind: kind,
                    category,
                    source: "tools",
                    input_index: None,
                }],
                outputs: vec![],
                partial: false,
            },
        },
    );
}

fn call(
    hub: &LiveHub,
    request_id: Uuid,
    response: &str,
    id: &str,
    name: &str,
    kind: ToolKind,
    arguments: &str,
) -> TextKey {
    let key = TextKey {
        request_id,
        response_id: Some(response.into()),
        wire_item_id: id.into(),
        content_index: 0,
    };
    let identity = ToolIdentity {
        tool_kind: kind,
        call_id: Some(id.into()),
        name: Some(name.into()),
        namespace: None,
        invalid_fields: false,
    };
    emit(
        hub,
        request_id,
        Change::Tool {
            update: ToolUpdate {
                key: key.clone(),
                fingerprint: fingerprint(&identity, arguments),
                identity,
                operation: ToolOperation::Complete,
                text: RedactionPolicy::new(vec![]).unwrap().scrub(arguments),
            },
        },
    );
    key
}

#[test]
fn websocket_turns_use_their_own_tool_context_and_native_evidence_domain() {
    let hub = LiveHub::new(LiveLimits::default());
    let id = Uuid::new_v4();
    let thread = Uuid::new_v4();
    request(&hub, id, Some(2), Some("first"), thread, "turn-first");
    context(
        &hub,
        id,
        Some(2),
        "exec_command",
        ToolKind::Function,
        ToolCategory::Command,
    );
    request(&hub, id, Some(3), Some("second"), thread, "turn-second");
    context(
        &hub,
        id,
        Some(3),
        "apply_patch",
        ToolKind::Custom,
        ToolCategory::Patch,
    );
    let first = call(
        &hub,
        id,
        "first",
        "same-call-id",
        "exec_command",
        ToolKind::Function,
        r#"{"cmd":"echo first"}"#,
    );
    let second = call(
        &hub,
        id,
        "second",
        "same-call-id",
        "apply_patch",
        ToolKind::Custom,
        "*** Begin Patch\n*** Add File: safe.txt\n+ok\n*** End Patch",
    );
    let unknown = call(
        &hub,
        id,
        "unmatched",
        "unmatched-call",
        "exec_command",
        ToolKind::Function,
        r#"{"cmd":"echo unknown"}"#,
    );
    let state = hub.state.lock().unwrap();
    assert_eq!(state.tools[0].category, ToolCategory::Command);
    assert_eq!(state.tools[0].command.as_ref().unwrap().text, "echo first");
    assert_eq!(state.tools[1].category, ToolCategory::Patch);
    assert!(state.tools[1].proposed_patch.is_some());
    assert_eq!(state.tools[2].category, ToolCategory::Other);
    assert!(request_for_key(&state, &unknown).is_none());
    assert_eq!(
        request_for_key(&state, &first)
            .unwrap()
            .info
            .codex_turn_id
            .as_deref(),
        Some("turn-first")
    );
    assert_eq!(
        request_for_key(&state, &second)
            .unwrap()
            .info
            .codex_turn_id
            .as_deref(),
        Some("turn-second")
    );
    // Both native command and native patch matching share this exact index.
    let indices = super::super::native_tools::call_indices(&state);
    for (turn, index) in [("turn-first", 0), ("turn-second", 1)] {
        assert_eq!(
            indices.get(&UserKey {
                codex_thread_id: thread,
                codex_turn_id: turn.into(),
                native_item_id: "same-call-id".into()
            }),
            Some(&vec![index])
        );
    }
    assert_eq!(indices.len(), 2);
}

#[test]
fn websocket_outputs_wait_for_verified_create_binding_and_join_only_matching_fingerprint() {
    let hub = LiveHub::new(LiveLimits::default());
    let id = Uuid::new_v4();
    let thread = Uuid::new_v4();
    request(&hub, id, Some(1), Some("first"), thread, "turn");
    context(
        &hub,
        id,
        Some(1),
        "exec_command",
        ToolKind::Function,
        ToolCategory::Command,
    );
    call(
        &hub,
        id,
        "first",
        "command",
        "exec_command",
        ToolKind::Function,
        r#"{"cmd":"echo first"}"#,
    );
    request(&hub, id, Some(2), None, thread, "turn");
    let body = json!({"input":[
        {"type":"function_call","call_id":"command","name":"exec_command","arguments":"{\"cmd\":\"echo first\"}"},
        {"type":"function_call_output","call_id":"command","output":"verified result"}
    ]});
    emit(
        &hub,
        id,
        Change::ToolContext {
            context: extract(&body, Some(2), &RedactionPolicy::new(vec![]).unwrap()),
        },
    );
    assert!(hub.state.lock().unwrap().tools[0].result.is_none());
    request(&hub, id, Some(2), Some("second"), thread, "turn");
    let state = hub.state.lock().unwrap();
    let result = state.tools[0].result.as_ref().unwrap();
    assert_eq!(result.output, "verified result");
    assert!(matches!(
        result.source,
        ToolResultSource::ModelRequest {
            client_request_index: Some(2),
            ..
        }
    ));
    assert!(!state.tools[0].result_conflict);
}

#[test]
fn http_stays_supported_and_ambiguous_websocket_response_never_uses_latest_request() {
    let hub = LiveHub::new(LiveLimits::default());
    let http = Uuid::new_v4();
    let thread = Uuid::new_v4();
    request(&hub, http, None, None, thread, "http-turn");
    context(
        &hub,
        http,
        None,
        "exec_command",
        ToolKind::Function,
        ToolCategory::Command,
    );
    call(
        &hub,
        http,
        "http-response",
        "http-call",
        "exec_command",
        ToolKind::Function,
        r#"{"cmd":"echo http"}"#,
    );
    let ws = Uuid::new_v4();
    request(&hub, ws, Some(1), Some("duplicate"), thread, "first");
    request(&hub, ws, Some(2), Some("duplicate"), thread, "second");
    context(
        &hub,
        ws,
        Some(2),
        "exec_command",
        ToolKind::Function,
        ToolCategory::Command,
    );
    let key = call(
        &hub,
        ws,
        "duplicate",
        "ws-call",
        "exec_command",
        ToolKind::Function,
        r#"{"cmd":"echo ws"}"#,
    );
    let state = hub.state.lock().unwrap();
    assert_eq!(state.tools[0].category, ToolCategory::Command);
    assert_eq!(state.tools[1].category, ToolCategory::Other);
    assert!(request_for_key(&state, &key).is_none());
    assert_eq!(super::super::native_tools::call_indices(&state).len(), 1);
}
