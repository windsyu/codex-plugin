use super::native_tools::{emit, request_domain};
use super::*;
use crate::workbench::decode::TextKey;
use crate::workbench::decode::tool::{self, ToolOperation, ToolUpdate};
use crate::workbench::decode::tool_context;
use crate::workbench::live::ExecutionState;

fn record(thread: Uuid, turn: &str, id: &str, status: &str) -> Value {
    json!({"type":"event_msg","ordinal":9,"payload":{"type":"item_completed","thread_id":thread,"turn_id":turn,"item":{"type":"FileChange","id":id,"status":status,"changes":{"/synthetic/file":{"type":"update","unified_diff":"not exported native body","move_path":"/synthetic/renamed"}},"stdout":"<img onerror=alert(1)> literal","stderr":"private-fixture-value"}}})
}
fn model_call(
    hub: &LiveHub,
    thread: Uuid,
    turn: &str,
    id: &str,
    namespace: &str,
    name: &str,
) -> Uuid {
    let request = request_domain(hub, thread, turn);
    let raw = "*** Begin Patch\n*** Add File: file\n+new\n*** End Patch";
    let item = json!({"type":"custom_tool_call","call_id":id,"name":name,"namespace":namespace,"input":raw});
    let definitions = json!({"tools":[{"type":"namespace","name":namespace,"tools":[{"type":"custom","name":name,"format":{"type":"grammar"}}]}]});
    emit(
        hub,
        request,
        Change::ToolContext {
            context: tool_context::extract(&definitions, None, &policy()),
        },
    );
    let kind = tool::kind(&item).unwrap();
    let identity = tool::identity(&item, kind, &policy());
    emit(
        hub,
        request,
        Change::Tool {
            update: ToolUpdate {
                key: TextKey {
                    request_id: request,
                    response_id: Some("resp".into()),
                    wire_item_id: format!("wire-{id}"),
                    content_index: 0,
                },
                fingerprint: tool::fingerprint(&identity, raw),
                identity,
                operation: ToolOperation::Complete,
                text: policy().scrub_tool(raw),
            },
        },
    );
    request
}
#[test]
fn file_changes_arrive_before_or_after_calls_and_repeats_keep_identity_proposal_and_sources() {
    for early in [false, true] {
        let (_dir, hub, mut reader, thread, path) = setup();
        target(&hub, thread, "turn");
        if !early {
            model_call(&hub, thread, "turn", "call", "functions", "apply_patch");
        }
        append(&path, &record(thread, "turn", "call", "completed"));
        reader.tick(&hub);
        if early {
            assert!(hub.snapshot().tools().is_empty());
            model_call(&hub, thread, "turn", "call", "functions", "apply_patch");
        }
        let snapshot = hub.snapshot();
        let call = &snapshot.tools()[0];
        assert_eq!(call.execution, ExecutionState::Succeeded);
        assert!(call.proposed_patch.is_some());
        let result = call.result.as_ref().unwrap();
        assert_eq!(
            result.streams.as_ref().unwrap().stdout.as_deref(),
            Some("<img onerror=alert(1)> literal")
        );
        assert_eq!(
            result.streams.as_ref().unwrap().stderr.as_deref(),
            Some("[已脱敏]")
        );
        assert!(result.exit_code.is_none());
        assert!(result.duration_ms.is_none());
        assert_eq!(
            serde_json::to_value(result).unwrap()["source"]["kind"],
            "native_rollout"
        );
        let safe = serde_json::to_string(&snapshot).unwrap();
        assert!(!safe.contains("private-fixture-value"));
        assert!(!safe.contains("not exported native body"));
        assert!(!safe.contains("fingerprint"));
        append(&path, &record(thread, "turn", "call", "completed"));
        reader.tick(&hub);
        assert_eq!(hub.snapshot().view_seq, snapshot.view_seq);
    }
}
#[test]
fn file_change_statuses_require_exact_turn_unique_call_and_native_patch_kind() {
    let (_dir, hub, mut reader, thread, path) = setup();
    model_call(&hub, thread, "one", "same", "functions", "apply_patch");
    model_call(&hub, thread, "two", "same", "functions", "apply_patch");
    model_call(&hub, thread, "one", "code", "functions", "exec");
    model_call(&hub, thread, "one", "foreign", "external", "apply_patch");
    append(&path, &record(thread, "two", "same", "failed"));
    append(&path, &record(thread, "one", "code", "completed"));
    append(&path, &record(thread, "one", "foreign", "completed"));
    reader.tick(&hub);
    let snapshot = hub.snapshot();
    let calls = snapshot.tools();
    assert_eq!(calls[0].execution, ExecutionState::Unobserved);
    assert_eq!(calls[1].execution, ExecutionState::Failed);
    assert_eq!(calls[2].execution, ExecutionState::Unobserved);
    assert_eq!(calls[3].execution, ExecutionState::Unobserved);
    append(&path, &record(thread, "one", "same", "declined"));
    reader.tick(&hub);
    assert_eq!(
        hub.snapshot().tools()[0].execution,
        ExecutionState::Declined
    );
    // A second actual call with the same native key destroys uniqueness.
    model_call(&hub, thread, "one", "same", "functions", "apply_patch");
    let snapshot = hub.snapshot();
    let calls = snapshot.tools();
    assert!(calls[0].result_conflict);
    assert!(calls[4].result_conflict);
    assert_eq!(calls[0].execution, ExecutionState::Unobserved);
    assert_eq!(calls[4].execution, ExecutionState::Unobserved);
}
#[test]
fn contradictory_file_change_records_preserve_both_sources_without_last_writer_success() {
    let (_dir, hub, mut reader, thread, path) = setup();
    model_call(&hub, thread, "turn", "call", "functions", "apply_patch");
    append(&path, &record(thread, "turn", "call", "failed"));
    reader.tick(&hub);
    let before = hub.snapshot().tools()[0].clone();
    append(&path, &record(thread, "turn", "call", "completed"));
    reader.tick(&hub);
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.native_file_changes.len(), 2);
    assert!(
        snapshot.native_file_changes[0].source.byte_offset
            < snapshot.native_file_changes[1].source.byte_offset
    );
    assert_eq!(snapshot.tools()[0].result, before.result);
    assert_eq!(snapshot.tools()[0].key, before.key);
    assert!(snapshot.tools()[0].result_conflict);
    assert_eq!(snapshot.tools()[0].execution, ExecutionState::Unobserved);
    assert_eq!(snapshot.capture, "partial");
}

#[test]
fn changed_native_tool_definition_revokes_previously_inferred_success_but_keeps_the_evidence() {
    let (_dir, hub, mut reader, thread, path) = setup();
    let request = model_call(&hub, thread, "turn", "call", "functions", "apply_patch");
    append(&path, &record(thread, "turn", "call", "completed"));
    reader.tick(&hub);
    let result = hub.snapshot().tools()[0].result.clone();
    emit(
        &hub,
        request,
        Change::ToolContext {
            context: tool_context::extract(&json!({"tools":[]}), None, &policy()),
        },
    );
    let snapshot = hub.snapshot();
    let call = &snapshot.tools()[0];
    assert!(call.result_conflict);
    assert_eq!(call.execution, ExecutionState::Unobserved);
    assert!(call.proposed_patch.is_none());
    assert_eq!(call.result, result);
    assert_eq!(snapshot.native_file_changes.len(), 1);
}
#[test]
fn invalid_file_change_evidence_is_diagnostic_and_turn_abort_or_started_is_not_a_tool_final() {
    let (_dir, hub, mut reader, thread, path) = setup();
    model_call(&hub, thread, "turn", "call", "functions", "apply_patch");
    let mut future = record(thread, "turn", "call", "future-status");
    append(&path, &future);
    future["payload"]["item"]["status"] = json!("completed");
    future["payload"]["thread_id"] = json!(Uuid::new_v4());
    append(&path, &future);
    future = record(thread, "turn", "call", "completed");
    future["payload"]["turn_id"] = Value::Null;
    append(&path, &future);
    future = record(thread, "turn", "call", "completed");
    future["payload"]["type"] = json!("item_started");
    append(&path, &future);
    append(
        &path,
        &json!({"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn","reason":"interrupted"}}),
    );
    reader.tick(&hub);
    let snapshot = hub.snapshot();
    assert!(snapshot.native_file_changes.is_empty());
    assert!(snapshot.tools()[0].result.is_none());
    assert_eq!(snapshot.tools()[0].execution, ExecutionState::Unobserved);
    assert_eq!(snapshot.user_capture.diagnostics.len(), 3);
}
#[test]
fn file_change_previews_preserve_empty_vs_missing_bound_streams_and_keep_unknown_paths_literal() {
    let thread = Uuid::new_v4();
    let source = Uuid::new_v4();
    let mut value = record(thread, "turn", "call", "failed");
    value["payload"]["item"]["stdout"] = json!("");
    value["payload"]["item"]["stderr"] = Value::Null;
    value["payload"]["item"]["changes"] =
        json!({"../../<script>.txt":{"type":"future","private_payload":"never export"}});
    let parsed = patches::file_change(&value, thread, source, 12, &policy())
        .unwrap()
        .unwrap();
    assert_eq!(parsed.stdout.as_deref(), Some(""));
    assert!(parsed.stderr.is_none());
    assert!(parsed.omitted);
    assert_eq!(
        parsed.files[0].operation,
        patches::NativeFileOperation::Unknown
    );
    assert_eq!(parsed.files[0].path, "../../<script>.txt");
    assert!(
        !serde_json::to_string(&parsed)
            .unwrap()
            .contains("never export")
    );
    value["payload"]["item"]["stdout"] = json!("a".repeat(70000));
    value["payload"]["item"]["stderr"] = json!("also bounded");
    let parsed = patches::file_change(&value, thread, source, 12, &policy())
        .unwrap()
        .unwrap();
    assert!(parsed.truncated);
    assert_eq!(parsed.stderr.as_deref(), Some("also bounded"));
    assert!(parsed.stdout.unwrap().len() + parsed.stderr.unwrap().len() <= 65536);
    value["payload"]["item"]["stdout"] = json!("... 2 bytes omitted ...\n");
    assert!(
        patches::file_change(&value, thread, source, 12, &policy())
            .unwrap()
            .unwrap()
            .truncated
    );
}
#[test]
fn pending_file_changes_wait_for_metadata_and_native_cache_eviction_stays_explicit() {
    let (_dir, hub, mut reader, thread, path) = setup();
    target(&hub, thread, "one");
    append(&path, &record(thread, "two", "future", "completed"));
    reader.tick(&hub);
    assert!(hub.snapshot().native_file_changes.is_empty());
    assert_eq!(reader.pending_file_changes.len(), 1);
    model_call(&hub, thread, "two", "future", "functions", "apply_patch");
    reader.tick(&hub);
    assert_eq!(
        hub.snapshot().tools()[0].execution,
        ExecutionState::Succeeded
    );
    for i in 0..129 {
        append(
            &path,
            &record(thread, "two", &format!("call-{i}"), "completed"),
        );
    }
    reader.tick(&hub);
    assert!(hub.snapshot().native_file_changes.len() <= 128);
    assert_eq!(hub.snapshot().capture, "partial");
}
