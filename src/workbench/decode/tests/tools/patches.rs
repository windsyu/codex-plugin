use super::*;
use crate::workbench::decode::patch::{PatchIssue, ProposedPatch};
use crate::workbench::live::ExecutionState;

const PATCH: &str = "*** Begin Patch\n*** Add File: new.txt\n+fixture-secret-value\n+<img onerror=alert(1)>\n*** End Patch";
fn definition(namespace: &str) -> Value {
    json!([{"type":"namespace","name":namespace,"tools":[{"type":"custom","name":"apply_patch","format":{"type":"grammar"}}]}])
}
#[tokio::test]
async fn patch_stream_final_and_late_result_preserve_identity_and_separate_execution() {
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits::default());
    let (id, thread) = (Uuid::new_v4(), Uuid::new_v4());
    request(
        &mut decoder,
        &hub,
        id,
        json!({"tools":definition("functions"),"client_metadata":metadata(thread,"turn")}),
    )
    .await;
    let (sender, mut receiver) = channel();
    let mut stream = sender.stream(Source {
        request_id: id,
        ..source()
    });
    for value in [
        json!({"type":"response.created","response":{"id":"resp_tools"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":call("patch","apply_patch",true,"")}),
        json!({"type":"response.custom_tool_call_input.delta","item_id":"item_patch","delta":PATCH}),
    ] {
        stream.offer(sse(value), Instant::now());
        for event in receive(&mut receiver, &mut decoder).await {
            hub.apply(event);
        }
    }
    let partial = hub.snapshot().tools()[0].clone();
    assert_eq!(partial.category, tool_context::ToolCategory::Patch);
    assert!(partial.proposed_patch.is_none());
    let item = call("patch", "apply_patch", true, PATCH);
    for value in complete(&item).into_iter().skip(1) {
        stream.offer(sse(value), Instant::now());
        for event in receive(&mut receiver, &mut decoder).await {
            hub.apply(event);
        }
    }
    let final_call = hub.snapshot().tools()[0].clone();
    assert_eq!(final_call.key, partial.key);
    assert_eq!(final_call.execution, ExecutionState::Unobserved);
    let Some(ProposedPatch::Ready { files, .. }) = &final_call.proposed_patch else {
        panic!("expected safe proposal")
    };
    assert_eq!(files[0].added_lines, 2);
    assert_eq!(files[0].sections[0].lines[0].text, "[已脱敏]");
    assert!(
        !serde_json::to_string(&hub.snapshot())
            .unwrap()
            .contains("fixture-secret-value")
    );
    request(&mut decoder,&hub,Uuid::new_v4(),json!({"client_metadata":metadata(thread,"turn"),"input":[item,{"type":"custom_tool_call_output","call_id":"patch","output":"Success. Files updated."}]})).await;
    let result = hub.snapshot().tools()[0].clone();
    assert_eq!(result.execution, ExecutionState::ResultObserved);
    assert_eq!(result.key, partial.key);
    assert_eq!(result.proposed_patch, final_call.proposed_patch);
    let revision = result.revision;
    response(&mut decoder, &hub, id, complete(&item)).await;
    assert_eq!(hub.snapshot().tools().len(), 1);
    assert_eq!(hub.snapshot().tools()[0].revision, revision);
}

#[tokio::test]
async fn patch_requires_unambiguous_native_definition_and_does_not_scan_code() {
    for namespace in ["foreign", "functions"] {
        let mut decoder = decoder();
        let hub = LiveHub::new(LiveLimits::default());
        let id = Uuid::new_v4();
        response(
            &mut decoder,
            &hub,
            id,
            complete(&call("patch", "apply_patch", true, PATCH)),
        )
        .await;
        assert!(hub.snapshot().tools()[0].proposed_patch.is_none());
        request(
            &mut decoder,
            &hub,
            id,
            json!({"tools":definition(namespace)}),
        )
        .await;
        assert_eq!(
            hub.snapshot().tools()[0].proposed_patch.is_some(),
            namespace == "functions"
        );
        let mut ambiguous = definition("functions").as_array().unwrap().clone();
        ambiguous.extend(definition("foreign").as_array().unwrap().clone());
        request(&mut decoder, &hub, id, json!({"tools":ambiguous})).await;
        assert_eq!(
            hub.snapshot().tools()[0].category,
            tool_context::ToolCategory::Other
        );
        assert!(hub.snapshot().tools()[0].proposed_patch.is_none());
        let code_id = Uuid::new_v4();
        request(&mut decoder, &hub, code_id, json!({"input":definitions()})).await;
        response(
            &mut decoder,
            &hub,
            code_id,
            complete(&call(
                "code",
                "exec",
                true,
                &format!("text(await tools.apply_patch({PATCH:?}));"),
            )),
        )
        .await;
        assert_eq!(
            hub.snapshot().tools()[1].category,
            tool_context::ToolCategory::Code
        );
        assert!(hub.snapshot().tools()[1].proposed_patch.is_none());
    }
}

#[tokio::test]
async fn incomplete_truncated_conflicting_and_redaction_omitted_patches_have_no_partial_diff() {
    for (limit, reason) in [
        (20, PatchIssue::Truncated),
        (65536, PatchIssue::IdentityConflict),
    ] {
        let mut decoder = decoder();
        let hub = LiveHub::new(LiveLimits {
            item_bytes: limit,
            ..LiveLimits::default()
        });
        let id = Uuid::new_v4();
        request(
            &mut decoder,
            &hub,
            id,
            json!({"tools":definition("functions")}),
        )
        .await;
        response(
            &mut decoder,
            &hub,
            id,
            complete(&call("patch", "apply_patch", true, PATCH)),
        )
        .await;
        if limit > 20 {
            response(
                &mut decoder,
                &hub,
                id,
                complete(&call(
                    "patch",
                    "apply_patch",
                    true,
                    &PATCH.replace("new.txt", "changed.txt"),
                )),
            )
            .await;
        }
        assert_eq!(
            hub.snapshot().tools()[0].proposed_patch,
            Some(ProposedPatch::Unavailable { reason })
        );
    }
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits::default());
    let id = Uuid::new_v4();
    request(
        &mut decoder,
        &hub,
        id,
        json!({"tools":definition("functions")}),
    )
    .await;
    response(&mut decoder,&hub,id,vec![json!({"type":"response.created","response":{"id":"resp_tools"}}),json!({"type":"response.output_item.added","output_index":0,"item":call("partial","apply_patch",true,"")}),json!({"type":"response.custom_tool_call_input.delta","item_id":"item_partial","delta":PATCH})]).await;
    assert_eq!(
        hub.snapshot().tools()[0].proposed_patch,
        Some(ProposedPatch::Unavailable {
            reason: PatchIssue::Incomplete
        })
    );
    let id = Uuid::new_v4();
    request(
        &mut decoder,
        &hub,
        id,
        json!({"tools":definition("functions")}),
    )
    .await;
    response(
        &mut decoder,
        &hub,
        id,
        complete(&call(
            "secret",
            "apply_patch",
            true,
            "*** Begin Patch\n*** Add File: config\n+token=synthetic-private\n*** End Patch",
        )),
    )
    .await;
    let snapshot = hub.snapshot();
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("synthetic-private")
    );
    assert!(matches!(
        snapshot.tools()[1].proposed_patch,
        Some(ProposedPatch::Unavailable { .. })
    ));
}

#[tokio::test]
async fn structured_patch_memory_counts_toward_the_shared_view_budget() {
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits {
        item_bytes: 400,
        view_bytes: 400,
        ..LiveLimits::default()
    });
    let id = Uuid::new_v4();
    request(
        &mut decoder,
        &hub,
        id,
        json!({"tools":definition("functions")}),
    )
    .await;
    response(
        &mut decoder,
        &hub,
        id,
        complete(&call("patch", "apply_patch", true, PATCH)),
    )
    .await;
    assert!(hub.snapshot().tools().is_empty());
    assert_eq!(hub.snapshot().capture, "partial");
}
