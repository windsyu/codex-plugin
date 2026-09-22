use super::*;
use crate::workbench::decode::TextKey;
use crate::workbench::decode::tool::{self, ToolOperation, ToolUpdate};
use crate::workbench::decode::tool_context;
use crate::workbench::live::ExecutionState;

fn command(thread: Uuid, turn: &str, id: &str, code: i32, output: &str) -> Value {
    json!({"ordinal":8,"type":"event_msg","payload":{"type":"item_completed","thread_id":thread,"turn_id":turn,"item":{
        "type":"CommandExecution","id":id,"process_id":"41","command":["/bin/sh","-c","printf synthetic"],"cwd":"file:///synthetic",
        "source":"unified_exec_startup","status":if code==0 {"completed"} else {"failed"},"exit_code":code,
        "duration":{"secs":1,"nanos":500000000},"aggregated_output":output
    }}})
}
pub(super) fn emit(hub: &LiveHub, id: Uuid, change: Change) {
    hub.apply(Decoded {
        request_id: id,
        capture_seq: 1,
        received_at: Instant::now(),
        change,
    });
}
pub(super) fn request_domain(hub: &LiveHub, thread: Uuid, turn: &str) -> Uuid {
    let id = Uuid::new_v4();
    emit(
        hub,
        id,
        Change::Request {
            info: RequestInfo {
                client_request_index: None,
                requested_model: None,
                codex_thread_id: Some(thread),
                codex_turn_id: Some(turn.into()),
                purpose: RequestPurpose::Conversation,
                purpose_basis: PurposeBasis::CodexTurnMetadata,
            },
        },
    );
    id
}
fn model_call(hub: &LiveHub, thread: Uuid, turn: &str, call: &str, custom: bool) -> Uuid {
    let id = request_domain(hub, thread, turn);
    let item = if custom {
        json!({"type":"custom_tool_call","call_id":call,"name":"exec","input":"text(await tools.exec_command({cmd:'printf synthetic'}))"})
    } else {
        json!({"type":"function_call","call_id":call,"name":"exec_command","arguments":"{\"cmd\":\"printf synthetic\"}"})
    };
    let definitions = json!({"tools":[{"type":"function","name":"exec_command","parameters":{"type":"object","properties":{"cmd":{"type":"string"}}}},{"type":"custom","name":"exec","format":{"type":"grammar"}}]});
    emit(
        hub,
        id,
        Change::ToolContext {
            context: tool_context::extract(&definitions, None, &policy()),
        },
    );
    let kind = tool::kind(&item).unwrap();
    let identity = tool::identity(&item, kind, &policy());
    let raw = tool::arguments(&item, kind).unwrap();
    emit(
        hub,
        id,
        Change::Tool {
            update: ToolUpdate {
                key: TextKey {
                    request_id: id,
                    response_id: Some("response".into()),
                    wire_item_id: format!("wire-{call}"),
                    content_index: 0,
                },
                fingerprint: tool::fingerprint(&identity, raw),
                identity,
                operation: ToolOperation::Complete,
                text: policy().scrub_tool(raw),
            },
        },
    );
    id
}
fn network_running(hub: &LiveHub, thread: Uuid, turn: &str, process: &str) {
    network_result(
        hub,
        thread,
        turn,
        &format!(
            "Wall time: 1 seconds\nProcess running with session ID {process}\nOutput:\npartial"
        ),
    );
}
fn network_result(hub: &LiveHub, thread: Uuid, turn: &str, output: &str) {
    let id = request_domain(hub, thread, turn);
    let input = json!({"input":[{"type":"function_call","call_id":"call-a","name":"exec_command","arguments":"{\"cmd\":\"printf synthetic\"}"},
        {"type":"function_call_output","call_id":"call-a","output":output}]});
    emit(
        hub,
        id,
        Change::ToolContext {
            context: tool_context::extract(&input, None, &policy()),
        },
    );
}

#[test]
fn native_final_result_arriving_before_call_keeps_source_and_updates_once() {
    let (_dir, hub, mut reader, thread, path) = setup();
    target(&hub, thread, "turn-a");
    let result = command(thread, "turn-a", "call-a", 7, "final native output");
    append(&path, &result);
    reader.tick(&hub);
    assert_eq!(hub.snapshot().native_commands.len(), 1);
    assert!(hub.snapshot().tools().is_empty());
    model_call(&hub, thread, "turn-a", "call-a", false);
    let snapshot = hub.snapshot();
    let tool = &snapshot.tools()[0];
    assert_eq!(tool.execution, ExecutionState::Failed);
    assert_eq!(tool.result.as_ref().unwrap().output, "final native output");
    let result = serde_json::to_value(tool.result.as_ref().unwrap()).unwrap();
    assert_eq!(result["source"]["kind"], "native_rollout");
    assert!(result["source"].get("requestId").is_none());
    let seq = snapshot.view_seq;
    append(
        &path,
        &command(thread, "turn-a", "call-a", 7, "final native output"),
    );
    reader.tick(&hub);
    assert_eq!(hub.snapshot().view_seq, seq);
    network_running(&hub, thread, "turn-a", "41");
    assert_eq!(hub.snapshot().tools()[0].execution, ExecutionState::Failed);
    assert_eq!(
        hub.snapshot().tools()[0].result.as_ref().unwrap().output,
        "final native output"
    );
}

#[test]
fn final_command_does_not_wait_for_next_request_or_mark_other_turns_and_code_children_complete() {
    let (_dir, hub, mut reader, thread, path) = setup();
    model_call(&hub, thread, "turn-a", "call-a", false);
    model_call(&hub, thread, "turn-b", "call-a", false);
    model_call(&hub, thread, "turn-a", "code-parent", true);
    append(&path, &command(thread, "turn-b", "call-a", 0, "other turn"));
    append(
        &path,
        &command(thread, "turn-a", "code-parent:0", 0, "nested output"),
    );
    reader.tick(&hub);
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.tools()[0].execution, ExecutionState::Unobserved);
    assert_eq!(snapshot.tools()[1].execution, ExecutionState::Succeeded);
    assert_eq!(snapshot.tools()[2].execution, ExecutionState::Unobserved);
    assert_eq!(snapshot.native_commands.len(), 2);
    append(
        &path,
        &command(thread, "turn-a", "call-a", 7, "late output"),
    );
    reader.tick(&hub);
    assert_eq!(hub.snapshot().tools()[0].execution, ExecutionState::Failed);
    assert_eq!(hub.snapshot().tools().len(), 3);
}

#[test]
fn conflicts_keep_both_native_records_and_never_use_last_arrival_as_success() {
    let (_dir, hub, mut reader, thread, path) = setup();
    model_call(&hub, thread, "turn-a", "call-a", false);
    append(
        &path,
        &command(thread, "turn-a", "call-a", 7, "first failure"),
    );
    reader.tick(&hub);
    append(
        &path,
        &command(thread, "turn-a", "call-a", 0, "contradictory success"),
    );
    reader.tick(&hub);
    let snapshot = hub.snapshot();
    assert_eq!(snapshot.native_commands.len(), 2);
    assert_eq!(snapshot.capture, "partial");
    assert!(snapshot.tools()[0].result_conflict);
    assert_eq!(snapshot.tools()[0].execution, ExecutionState::Unobserved);
    assert_eq!(
        snapshot.tools()[0].result.as_ref().unwrap().output,
        "first failure"
    );
}

#[test]
fn ambiguous_call_ids_and_wrong_process_ids_cannot_join_native_results() {
    for duplicate in [true, false] {
        let (_dir, hub, mut reader, thread, path) = setup();
        model_call(&hub, thread, "turn-a", "call-a", false);
        if duplicate {
            model_call(&hub, thread, "turn-a", "call-a", false);
        } else {
            network_running(&hub, thread, "turn-a", "99");
        }
        append(&path, &command(thread, "turn-a", "call-a", 0, "unmatched"));
        reader.tick(&hub);
        assert!(
            hub.snapshot()
                .tools()
                .iter()
                .all(|tool| tool.result_conflict && tool.execution == ExecutionState::Unobserved)
        );
    }
}

#[test]
fn invalid_and_future_evidence_is_diagnostic_and_turn_cancellation_is_not_tool_cancellation() {
    let (_dir, hub, mut reader, thread, path) = setup();
    model_call(&hub, thread, "turn-a", "call-a", false);
    append(
        &path,
        &command(Uuid::new_v4(), "turn-a", "call-a", 0, "wrong thread"),
    );
    let mut future = command(thread, "turn-a", "call-a", 0, "future");
    future["payload"]["item"]["status"] = json!("future_status");
    append(&path, &future);
    let mut invalid = command(thread, "turn-a", "call-a", 0, "invalid");
    invalid["payload"]["item"]["duration"]["nanos"] = json!(1000000000);
    append(&path, &invalid);
    append(
        &path,
        &json!({"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-a","reason":"interrupted"}}),
    );
    reader.tick(&hub);
    let snapshot = hub.snapshot();
    assert!(snapshot.native_commands.is_empty());
    assert_eq!(snapshot.tools()[0].execution, ExecutionState::Unobserved);
    for code in [
        UserIssue::IdentityConflict,
        UserIssue::UnsupportedToolEvidence,
        UserIssue::InvalidLine,
    ] {
        assert!(
            snapshot
                .user_capture
                .diagnostics
                .iter()
                .any(|d| d.code == code)
        );
    }
}

#[test]
fn native_record_previews_redact_before_clipping_and_remain_bounded() {
    let (_dir, hub, mut reader, thread, path) = setup();
    target(&hub, thread, "turn-a");
    append(
        &path,
        &command(
            thread,
            "turn-a",
            "call-a",
            0,
            &format!(
                "private-fixture-value Bearer synthetic\n{}",
                "中".repeat(30000)
            ),
        ),
    );
    reader.tick(&hub);
    let snapshot = hub.snapshot();
    let command = &snapshot.native_commands[0];
    assert!(command.truncated);
    assert!(command.output.len() <= 64 * 1024);
    let serialized = serde_json::to_string(&snapshot).unwrap();
    assert!(!serialized.contains("private-fixture-value"));
    assert!(!serialized.contains("Bearer synthetic"));
    for n in 0..140 {
        append(&path, &command_fixture(thread, n));
    }
    for _ in 0..6 {
        reader.tick(&hub);
    }
    assert!(hub.snapshot().native_commands.len() <= 128);
    assert_eq!(hub.snapshot().capture, "partial");
}
fn command_fixture(thread: Uuid, n: usize) -> Value {
    command(thread, "turn-a", &format!("call-{n}"), 0, "bounded")
}

#[test]
fn native_omission_marker_is_visible_even_when_the_retained_output_fits_the_preview() {
    let (_dir, hub, mut reader, thread, path) = setup();
    model_call(&hub, thread, "turn-a", "call-a", false);
    append(
        &path,
        &command(
            thread,
            "turn-a",
            "call-a",
            0,
            "head\n... 600 bytes omitted ...\ntail",
        ),
    );
    reader.tick(&hub);
    let snapshot = hub.snapshot();
    assert!(snapshot.native_commands[0].truncated);
    assert!(snapshot.tools()[0].result.as_ref().unwrap().truncated);
    assert_eq!(snapshot.tools()[0].execution, ExecutionState::Succeeded);
}

#[test]
fn explicit_started_never_regresses_a_final_result_and_declined_is_not_success() {
    let (_dir, hub, mut reader, thread, path) = setup();
    model_call(&hub, thread, "turn-a", "call-a", false);
    let mut started = command(thread, "turn-a", "call-a", 0, "");
    started["payload"]["type"] = json!("item_started");
    started["payload"]["item"]["status"] = json!("in_progress");
    started["payload"]["item"]["exit_code"] = Value::Null;
    started["payload"]["item"]["duration"] = Value::Null;
    append(&path, &started);
    reader.tick(&hub);
    assert_eq!(hub.snapshot().tools()[0].execution, ExecutionState::Running);
    let mut declined = command(thread, "turn-a", "call-a", -1, "declined by user");
    declined["payload"]["item"]["status"] = json!("declined");
    append(&path, &declined);
    reader.tick(&hub);
    assert_eq!(
        hub.snapshot().tools()[0].execution,
        ExecutionState::Declined
    );
    let seq = hub.snapshot().view_seq;
    append(&path, &started);
    reader.tick(&hub);
    assert_eq!(hub.snapshot().view_seq, seq);
    assert_eq!(
        hub.snapshot().tools()[0].execution,
        ExecutionState::Declined
    );
}

#[test]
fn native_and_network_exit_conflicts_are_not_resolved_by_arrival_order() {
    for native_first in [true, false] {
        let (_dir, hub, mut reader, thread, path) = setup();
        model_call(&hub, thread, "turn-a", "call-a", false);
        if native_first {
            append(
                &path,
                &command(thread, "turn-a", "call-a", 7, "native failure"),
            );
            reader.tick(&hub);
        }
        network_result(
            &hub,
            thread,
            "turn-a",
            "Wall time: 1 seconds\nProcess exited with code 0\nOutput:\nnetwork result",
        );
        if !native_first {
            append(
                &path,
                &command(thread, "turn-a", "call-a", 7, "native failure"),
            );
            reader.tick(&hub);
        }
        let snapshot = hub.snapshot();
        assert!(snapshot.tools()[0].result_conflict);
        assert_eq!(snapshot.tools()[0].execution, ExecutionState::Unobserved);
        assert_eq!(snapshot.native_commands.len(), 1);
        assert_eq!(
            snapshot.tool_contexts.last().unwrap().context.outputs.len(),
            1
        );
    }
}
