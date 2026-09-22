use super::*;
use crate::workbench::live::{LiveHub, LiveLimits};

mod patches;

fn metadata(thread: Uuid, turn: &str) -> Value {
    json!({"x-codex-turn-metadata":json!({"request_kind":"turn","thread_source":"user","thread_id":thread,"turn_id":turn}).to_string()})
}
fn definitions() -> Value {
    json!([{ "type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"functions","tools":[
        {"type":"custom","name":"exec","format":{"type":"grammar"}},
        {"type":"function","name":"exec_command","parameters":{"type":"object","properties":{"cmd":{"type":"string"},"workdir":{"type":"string"}}}}
    ]}]}])
}
async fn feed(decoder: &mut Decoder, hub: &LiveHub, source: Source, bytes: Bytes) {
    let (sender, mut receiver) = channel();
    let mut stream = sender.stream(source);
    stream.offer(bytes, Instant::now());
    for event in receive(&mut receiver, decoder).await {
        hub.apply(event);
    }
    stream.finish();
    for event in receive(&mut receiver, decoder).await {
        hub.apply(event);
    }
}
async fn request(decoder: &mut Decoder, hub: &LiveHub, id: Uuid, body: Value) {
    feed(
        decoder,
        hub,
        Source {
            request_id: id,
            direction: Direction::Request,
            transport: Transport::Http,
            content_type: "application/json".into(),
            content_encoding: String::new(),
        },
        Bytes::from(serde_json::to_vec(&body).unwrap()),
    )
    .await;
}
async fn response(decoder: &mut Decoder, hub: &LiveHub, id: Uuid, events: Vec<Value>) {
    let bytes = events
        .into_iter()
        .flat_map(|value| sse(value).to_vec())
        .collect::<Vec<_>>();
    feed(
        decoder,
        hub,
        Source {
            request_id: id,
            ..source()
        },
        Bytes::from(bytes),
    )
    .await;
}
fn call(id: &str, name: &str, custom: bool, text: &str) -> Value {
    if custom {
        json!({"type":"custom_tool_call","id":format!("item_{id}"),"call_id":id,"name":name,"input":text})
    } else {
        json!({"type":"function_call","id":format!("item_{id}"),"call_id":id,"name":name,"arguments":text})
    }
}
fn complete(item: &Value) -> Vec<Value> {
    vec![
        json!({"type":"response.created","response":{"id":"resp_tools"}}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":{"id":"resp_tools","output":[item]}}),
    ]
}

#[test]
fn result_context_marks_native_truncation_without_guessing_execution() {
    let policy = RedactionPolicy::new(vec![]).unwrap();
    for output in [
        "head\n... 600 bytes omitted ...\ntail",
        "Warning: truncated output (original token count: 21)\nTotal output lines: 2\n\nhead…11 tokens truncated…tail",
        "head…51 chars truncated…tail",
    ] {
        let context = tool_context::extract(
            &json!({"input":[{"type":"function_call_output","call_id":"call","output":output}]}),
            None,
            &policy,
        );
        assert!(context.outputs[0].truncated);
        assert!(context.outputs[0].command_facts.is_none());
    }
}

#[tokio::test]
async fn late_wire_ids_preserve_original_text_and_tool_keys_without_replaying_content() {
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits::default());
    let (sender, mut receiver) = channel();
    let mut stream = sender.stream(source());
    for value in [
        json!({"type":"response.created","response":{"id":"resp_tools"}}),
        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"中文前缀"}),
        json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"late-call","name":"exec_command","arguments":""}}),
        json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"cmd\":\"echo 中文\"}"}),
    ] {
        stream.offer(sse(value), Instant::now());
        for event in receive(&mut receiver, &mut decoder).await {
            hub.apply(event);
        }
    }
    let before = hub.snapshot();
    let text_key = before.model_items()[0].key.clone();
    let tool_key = before.tools()[0].key.clone();
    let final_tool = json!({"type":"function_call","id":"late-tool","call_id":"late-call","name":"exec_command","arguments":"{\"cmd\":\"echo 中文\"}"});
    let final_text = json!({"type":"message","role":"assistant","id":"late-text","content":[{"type":"output_text","text":"中文最终全文"}]});
    for value in [
        json!({"type":"response.output_text.done","item_id":"late-text","output_index":0,"content_index":0,"text":"中文最终全文"}),
        json!({"type":"response.output_item.done","output_index":1,"item":final_tool}),
        json!({"type":"response.function_call_arguments.delta","item_id":"late-tool","delta":"must not append after completion"}),
        json!({"type":"response.output_text.delta","item_id":"late-text","content_index":0,"delta":"must not append after completion"}),
        json!({"type":"response.completed","response":{"id":"resp_tools","output":[final_text,final_tool]}}),
    ] {
        stream.offer(sse(value), Instant::now());
        for event in receive(&mut receiver, &mut decoder).await {
            hub.apply(event);
        }
    }
    let after = hub.snapshot();
    assert_eq!(after.model_items().len(), 1);
    assert_eq!(after.tools().len(), 1);
    assert_eq!(after.model_items()[0].key, text_key);
    assert_eq!(after.tools()[0].key, tool_key);
    assert_eq!(after.model_items()[0].text, "中文最终全文");
    assert_eq!(after.tools()[0].arguments, "{\"cmd\":\"echo 中文\"}");
    assert_eq!(after.tools()[0].order_index, before.tools()[0].order_index);
    assert!(!after.tools()[0].identity_conflict);
}

#[tokio::test]
async fn invalid_explicit_ids_cannot_borrow_an_existing_output_slot_or_response() {
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits::default());
    response(&mut decoder, &hub, Uuid::new_v4(), vec![
        json!({"type":"response.created","response":{"id":"resp_tools"}}),
        json!({"type":"response.output_text.delta","item_id":"valid-item","output_index":0,"content_index":0,"delta":"safe"}),
        json!({"type":"response.output_text.delta","item_id":"fixture-secret-value","output_index":0,"content_index":0,"delta":"unsafe-invalid-id"}),
        json!({"type":"response.output_text.delta","item_id":"valid-item","output_index":0,"content_index":0,"response_id":"other-response","delta":"unsafe-response"}),
        json!({"type":"response.output_text.done","item_id":"valid-item","output_index":0,"content_index":0,"text":"safe final"}),
    ]).await;
    let view = hub.snapshot();
    assert_eq!(view.model_items().len(), 1);
    assert_eq!(view.model_items()[0].text, "safe final");
    assert_eq!(view.capture, "partial");
    let serialized = serde_json::to_string(&view).unwrap();
    assert!(!serialized.contains("unsafe-"));
    assert!(
        view.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::MissingIdentity)
    );
    assert!(
        view.diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::ConflictingIdentity)
    );
}

#[tokio::test]
async fn actual_shapes_stream_code_separately_and_complete_arguments_never_complete_execution() {
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits::default());
    let id = Uuid::new_v4();
    request(
        &mut decoder,
        &hub,
        id,
        json!({"client_metadata":metadata(Uuid::new_v4(),"turn-1"),"input":definitions()}),
    )
    .await;
    let (sender, mut receiver) = channel();
    let mut stream = sender.stream(Source {
        request_id: id,
        ..source()
    });
    let initial = call("call_a", "exec", true, "");
    let raw = "text(await tools.exec_command({cmd:'printf 中文 fixture-secret-value'}));";
    for value in [
        json!({"type":"response.created","response":{"id":"resp_tools"}}),
        delta("```sh\necho plain prose\n```"),
        json!({"type":"response.output_item.added","output_index":1,"item":initial}),
    ] {
        stream.offer(sse(value), Instant::now());
        for event in receive(&mut receiver, &mut decoder).await {
            hub.apply(event);
        }
    }
    let mut subscription = hub.subscribe(hub.epoch(), hub.snapshot().view_seq).unwrap();
    for ch in raw.chars() {
        stream.offer(sse(json!({"type":"response.custom_tool_call_input.delta","item_id":"item_call_a","delta":ch.to_string()})),Instant::now());
        for event in receive(&mut receiver, &mut decoder).await {
            hub.apply(event);
        }
        let view = hub.snapshot();
        assert_eq!(view.tools().len(), 1);
        assert!(view.tools()[0].result.is_none());
        assert!(
            !serde_json::to_string(&view)
                .unwrap()
                .contains("fixture-secret-value")
        );
    }
    assert_eq!(
        hub.snapshot().tools()[0].arguments_state,
        tool::ArgumentState::Receiving
    );
    let update: Value = serde_json::from_str(&subscription.recv().await.unwrap().json).unwrap();
    assert_eq!(update["kind"], "item.patch");
    assert_eq!(update["field"], "arguments");
    let full = call("call_a", "exec", true, raw);
    for value in [
        json!({"type":"response.custom_tool_call_input.done","item_id":"item_call_a","input":raw}),
        json!({"type":"response.output_item.done","item":full}),
        json!({"type":"response.completed","response":{"id":"resp_tools","output":[full]}}),
    ] {
        stream.offer(sse(value), Instant::now());
        for event in receive(&mut receiver, &mut decoder).await {
            hub.apply(event);
        }
    }
    let view = hub.snapshot();
    let call = &view.tools()[0];
    assert_eq!(call.arguments_state, tool::ArgumentState::Generated);
    assert_eq!(call.category, tool_context::ToolCategory::Code);
    assert_eq!(
        call.execution,
        crate::workbench::live::ExecutionState::Unobserved
    );
    assert!(call.command.is_none());
    assert!(call.result.is_none());
    assert!(call.arguments.contains("[已脱敏]"));
    assert_eq!(call.arguments.matches("text(await").count(), 1);
    assert_eq!(view.model_items().len(), 1);
    assert!(view.model_items()[0].order_index < call.order_index);
    let safe = serde_json::to_string(&view).unwrap();
    assert!(!safe.contains("fingerprint"));
    assert!(!safe.contains("commandFacts"));
}

#[tokio::test]
async fn results_need_native_thread_explicit_companion_and_unique_call_not_recent_activity() {
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits::default());
    let thread = Uuid::new_v4();
    let id = Uuid::new_v4();
    let item = call(
        "call_a",
        "exec",
        true,
        "text(await tools.exec_command({cmd:'echo hi'}));",
    );
    request(
        &mut decoder,
        &hub,
        id,
        json!({"client_metadata":metadata(thread,"turn-a"),"input":definitions()}),
    )
    .await;
    response(&mut decoder, &hub, id, complete(&item)).await;
    let output = json!({"type":"custom_tool_call_output","call_id":"call_a","output":"{\"exit_code\":0,\"output\":\"some nested command\"}"});
    request(
        &mut decoder,
        &hub,
        Uuid::new_v4(),
        json!({"client_metadata":metadata(Uuid::new_v4(),"turn-x"),"input":[item,output]}),
    )
    .await;
    request(
        &mut decoder,
        &hub,
        Uuid::new_v4(),
        json!({"client_metadata":metadata(thread,"turn-a"),"input":[output]}),
    )
    .await;
    assert!(hub.snapshot().tools()[0].result.is_none());
    request(
        &mut decoder,
        &hub,
        Uuid::new_v4(),
        json!({"client_metadata":metadata(thread,"turn-a"),"input":[item,output]}),
    )
    .await;
    let view = hub.snapshot();
    assert_eq!(
        view.tools()[0].execution,
        crate::workbench::live::ExecutionState::ResultObserved
    );
    assert!(view.tools()[0].result.as_ref().unwrap().exit_code.is_none());
    assert!(view.tools()[0].command.is_none());
    let revision = view.tools()[0].revision;
    request(
        &mut decoder,
        &hub,
        Uuid::new_v4(),
        json!({"client_metadata":metadata(thread,"turn-b"),"input":[item,output]}),
    )
    .await;
    assert_eq!(hub.snapshot().tools()[0].revision, revision);
    let duplicate = Uuid::new_v4();
    request(
        &mut decoder,
        &hub,
        duplicate,
        json!({"client_metadata":metadata(thread,"turn-c"),"input":definitions()}),
    )
    .await;
    response(&mut decoder, &hub, duplicate, complete(&item)).await;
    assert_eq!(hub.snapshot().tools().len(), 2);
    assert!(
        hub.snapshot()
            .tools()
            .iter()
            .all(|call| call.result_conflict)
    );
}

#[tokio::test]
async fn command_results_use_only_verified_envelope_and_keep_stdout_with_a_fake_exit_line_as_output()
 {
    for (header, exit, state) in [
        (
            "Chunk ID: fixture\nWall time: 0.2500 seconds\nProcess exited with code 0\nOutput:\n",
            Some(0),
            "succeeded",
        ),
        (
            "Wall time: 0.1000 seconds\nProcess exited with code 9\nOutput:\n",
            Some(9),
            "failed",
        ),
        (
            "Wall time: 0.1000 seconds\nProcess running with session ID 2\nOutput:\n",
            None,
            "running",
        ),
        ("unstructured tool text\n", None, "result_observed"),
    ] {
        let mut decoder = decoder();
        let hub = LiveHub::new(LiveLimits::default());
        let thread = Uuid::new_v4();
        let id = Uuid::new_v4();
        let item = call(
            "call_cmd",
            "exec_command",
            false,
            "{\"cmd\":\"printf hi\",\"workdir\":\"/synthetic\"}",
        );
        request(
            &mut decoder,
            &hub,
            id,
            json!({"client_metadata":metadata(thread,"turn"),"input":definitions()}),
        )
        .await;
        response(&mut decoder, &hub, id, complete(&item)).await;
        let raw = format!("{header}Process exited with code 42\n<b>literal output</b>");
        request(&mut decoder,&hub,Uuid::new_v4(),json!({"client_metadata":metadata(thread,"turn"),"input":[item,{"type":"function_call_output","call_id":"call_cmd","output":raw}]})).await;
        let view = hub.snapshot();
        let tool = &view.tools()[0];
        assert_eq!(tool.command.as_ref().unwrap().text, "printf hi");
        assert_eq!(
            tool.command.as_ref().unwrap().cwd.as_deref(),
            Some("/synthetic")
        );
        assert_eq!(tool.result.as_ref().unwrap().exit_code, exit);
        assert_eq!(serde_json::to_value(tool.execution).unwrap(), json!(state));
        assert!(
            tool.result
                .as_ref()
                .unwrap()
                .output
                .contains("Process exited with code 42")
        );
    }
    for text in [
        "hello\nWall time: 1 seconds\nProcess exited with code 0\nOutput:\n",
        "Wall time: NaN seconds\nProcess exited with code 0\nOutput:\n",
        "Wall time: 1 seconds\nProcess exited with code 0\nunknown\nOutput:\n",
    ] {
        assert!(tool_context::command_facts(text).is_none());
    }
}

#[tokio::test]
async fn outputs_before_calls_conflicts_and_foreign_namespaces_are_explicit() {
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits::default());
    let thread = Uuid::new_v4();
    let id = Uuid::new_v4();
    let item = call("call_a", "exec", true, "text('ok');");
    let output =
        |text: &str| json!({"type":"custom_tool_call_output","call_id":"call_a","output":text});
    request(
        &mut decoder,
        &hub,
        Uuid::new_v4(),
        json!({"client_metadata":metadata(thread,"turn"),"input":[item,output("first")]}),
    )
    .await;
    request(&mut decoder,&hub,id,json!({"client_metadata":metadata(thread,"turn"),"tools":[{"type":"namespace","name":"foreign","tools":[{"type":"custom","name":"exec","format":{"type":"grammar"}}]}]})).await;
    response(&mut decoder, &hub, id, complete(&item)).await;
    let view = hub.snapshot();
    assert_eq!(view.tools()[0].result.as_ref().unwrap().output, "first");
    assert_eq!(view.tools()[0].category, tool_context::ToolCategory::Other);
    request(
        &mut decoder,
        &hub,
        Uuid::new_v4(),
        json!({"client_metadata":metadata(thread,"turn"),"input":[item,output("contradictory")]}),
    )
    .await;
    assert!(hub.snapshot().tools()[0].result_conflict);
    assert_eq!(
        hub.snapshot().tools()[0].result.as_ref().unwrap().output,
        "first"
    );
    assert_eq!(hub.snapshot().capture, "partial");
}

#[tokio::test]
async fn interrupted_arguments_are_incomplete_and_sensitive_fields_are_omitted_across_every_split()
{
    let raw = "{\"cmd\":\"echo hi\",\"password\":\"synthetic-private\",\"after\":\"omit me too\"}";
    for split in 0..=raw.len() {
        let policy = RedactionPolicy::new(vec![]).unwrap();
        let mut redactor = TextRedactor::tool(policy);
        let mut safe = redactor.push(&raw[..split]).as_str().to_owned();
        safe.push_str(redactor.push(&raw[split..]).as_str());
        safe.push_str(redactor.finish().as_str());
        assert!(!safe.contains("synthetic-private"));
        assert!(!safe.contains("omit me too"));
    }
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits::default());
    let id = Uuid::new_v4();
    response(&mut decoder,&hub,id,vec![json!({"type":"response.created","response":{"id":"resp"}}),json!({"type":"response.output_item.added","item":call("call_a","exec",true,"")}),json!({"type":"response.custom_tool_call_input.delta","item_id":"item_call_a","delta":"partial arguments"}),json!({"type":"response.completed","response":{"id":"resp"}})]).await;
    assert_eq!(
        hub.snapshot().tools()[0].arguments_state,
        tool::ArgumentState::Incomplete
    );
    assert!(hub.snapshot().tools()[0].result.is_none());
}

#[tokio::test]
async fn tool_previews_share_reading_limits_and_structured_images_never_enter_the_view() {
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits {
        item_bytes: 5,
        view_bytes: 20,
        items: 1,
        ..LiveLimits::default()
    });
    let id = Uuid::new_v4();
    response(
        &mut decoder,
        &hub,
        id,
        complete(&call("call_a", "exec", true, "中文过长参数")),
    )
    .await;
    assert_eq!(hub.snapshot().tools()[0].arguments, "中");
    assert!(hub.snapshot().tools()[0].truncated);
    response(
        &mut decoder,
        &hub,
        Uuid::new_v4(),
        complete(&call("call_b", "exec", true, "other")),
    )
    .await;
    assert_eq!(hub.snapshot().tools().len(), 1);
    assert_eq!(hub.snapshot().capture, "partial");
    let context = tool_context::extract(
        &json!({"input":[{"type":"function_call_output","call_id":"call_a","output":[{"type":"input_image","image_url":"data:image/png;base64,PRIVATE_IMAGE"},{"type":"input_text","text":"visible"}]}]}),
        None,
        &RedactionPolicy::new(vec![]).unwrap(),
    );
    let safe = serde_json::to_string(&context).unwrap();
    assert!(!safe.contains("PRIVATE_IMAGE"));
    assert!(context.outputs[0].omitted);
    assert_eq!(context.outputs[0].output.as_str(), "visible");
}

#[tokio::test]
async fn websocket_tool_streams_require_response_identity_and_finish_independently() {
    let (sender, mut receiver) = channel();
    let id = Uuid::new_v4();
    let mut stream = sender.stream(Source {
        request_id: id,
        transport: Transport::WebSocket,
        content_type: String::new(),
        ..source()
    });
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits::default());
    for value in [
        json!({"type":"response.created","response":{"id":"resp_a"}}),
        json!({"type":"response.output_item.added","response_id":"resp_a","output_index":0,"item":{"type":"custom_tool_call","call_id":"shared_call","name":"exec","input":""}}),
        json!({"type":"response.created","response":{"id":"resp_b"}}),
        json!({"type":"response.output_item.added","response_id":"resp_b","output_index":0,"item":{"type":"custom_tool_call","call_id":"shared_call","name":"exec","input":""}}),
        json!({"type":"response.custom_tool_call_input.delta","item_id":"item_shared_call","delta":"no-response-must-not-appear"}),
        json!({"type":"response.custom_tool_call_input.delta","response_id":"resp_a","output_index":0,"delta":"alpha"}),
        json!({"type":"response.custom_tool_call_input.delta","response_id":"resp_b","output_index":0,"delta":"beta text!"}),
        json!({"type":"response.output_item.done","response_id":"resp_a","output_index":0,"item":call("shared_call","exec",true,"alpha complete")}),
        json!({"type":"response.completed","response":{"id":"resp_a"}}),
    ] {
        stream.offer(ws(value), Instant::now());
        for event in receive(&mut receiver, &mut decoder).await {
            hub.apply(event);
        }
    }
    let view = hub.snapshot();
    assert_eq!(view.tools().len(), 2);
    assert!(
        view.tools()
            .iter()
            .all(|tool| tool.key.wire_item_id == "@output:0")
    );
    assert_eq!(
        view.tools()[0].arguments_state,
        tool::ArgumentState::Generated
    );
    assert_eq!(
        view.tools()[1].arguments_state,
        tool::ArgumentState::Receiving
    );
    assert_eq!(view.tools()[1].arguments, "beta text!");
    assert!(
        !serde_json::to_string(&view)
            .unwrap()
            .contains("no-response-must-not-appear")
    );
    stream.finish();
    for event in receive(&mut receiver, &mut decoder).await {
        hub.apply(event);
    }
    assert_eq!(
        hub.snapshot().tools()[1].arguments_state,
        tool::ArgumentState::Incomplete
    );
}

#[tokio::test]
async fn gap_suppresses_unsafe_tool_suffixes_until_a_full_replacement_and_repeated_added_conflicts()
{
    let (sender, mut receiver) = channel();
    let mut stream = sender.stream(source());
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits::default());
    for value in [
        json!({"type":"response.created","response":{"id":"resp"}}),
        json!({"type":"response.output_item.added","item":call("call_a","exec",true,"")}),
    ] {
        stream.offer(sse(value), Instant::now());
        for event in receive(&mut receiver, &mut decoder).await {
            hub.apply(event);
        }
    }
    stream.offer(sse(json!({"type":"response.custom_tool_call_input.delta","item_id":"item_call_a","delta":"Bearer "})),Instant::now());
    drop(receiver.recv().await.unwrap());
    stream.offer(sse(json!({"type":"response.custom_tool_call_input.delta","item_id":"item_call_a","delta":"hidden-suffix"})),Instant::now());
    for event in receive(&mut receiver, &mut decoder).await {
        hub.apply(event);
    }
    assert_eq!(
        hub.snapshot().tools()[0].arguments_state,
        tool::ArgumentState::Incomplete
    );
    assert!(
        !serde_json::to_string(&hub.snapshot())
            .unwrap()
            .contains("hidden-suffix")
    );
    stream.offer(sse(json!({"type":"response.output_item.done","item":call("call_a","exec",true,"Bearer hidden-suffix")})),Instant::now());
    for event in receive(&mut receiver, &mut decoder).await {
        hub.apply(event);
    }
    assert_eq!(hub.snapshot().tools()[0].arguments, "[已脱敏]");
    assert_eq!(
        hub.snapshot().tools()[0].arguments_state,
        tool::ArgumentState::Generated
    );
    stream.offer(sse(json!({"type":"response.output_item.added","item":call("call_a","different_tool",true,"")})),Instant::now());
    for event in receive(&mut receiver, &mut decoder).await {
        hub.apply(event);
    }
    assert!(hub.snapshot().tools()[0].identity_conflict);
    assert_eq!(
        hub.snapshot().tools()[0].identity.name.as_deref(),
        Some("exec")
    );
}

#[tokio::test]
async fn reverse_parallel_results_and_replayed_full_items_keep_call_order_and_original_identity() {
    let mut decoder = decoder();
    let hub = LiveHub::new(LiveLimits::default());
    let thread = Uuid::new_v4();
    let id = Uuid::new_v4();
    let first = call("call_a", "exec", true, "text('first');");
    let second = call("call_b", "exec", true, "text('second');");
    request(
        &mut decoder,
        &hub,
        id,
        json!({"client_metadata":metadata(thread,"turn"),"input":definitions()}),
    )
    .await;
    response(
        &mut decoder,
        &hub,
        id,
        vec![
            json!({"type":"response.created","response":{"id":"resp"}}),
            json!({"type":"response.output_item.done","output_index":0,"item":first}),
            json!({"type":"response.output_item.done","output_index":1,"item":second}),
            json!({"type":"response.completed","response":{"id":"resp","output":[first,second]}}),
        ],
    )
    .await;
    let order = hub
        .snapshot()
        .tools()
        .iter()
        .map(|tool| (tool.key.clone(), tool.order_index))
        .collect::<Vec<_>>();
    request(&mut decoder,&hub,Uuid::new_v4(),json!({"client_metadata":metadata(thread,"turn"),"input":[first,second,{"type":"custom_tool_call_output","call_id":"call_b","output":"second result"},{"type":"custom_tool_call_output","call_id":"call_a","output":"first result"}]})).await;
    let view = hub.snapshot();
    assert_eq!(
        view.tools()[0].result.as_ref().unwrap().output,
        "first result"
    );
    assert_eq!(
        view.tools()[1].result.as_ref().unwrap().output,
        "second result"
    );
    assert_eq!(
        view.tools()
            .iter()
            .map(|tool| (tool.key.clone(), tool.order_index))
            .collect::<Vec<_>>(),
        order
    );
}

#[tokio::test]
async fn invalid_or_missing_final_identity_cannot_replace_previously_proven_arguments() {
    for invalid_field in [Some("namespace"), Some("call_id"), Some("name"), None] {
        let mut decoder = decoder();
        let hub = LiveHub::new(LiveLimits::default());
        let id = Uuid::new_v4();
        let item = call("call_a", "exec", true, "text('original');");
        let mut changed = call("call_a", "exec", true, "text('unproven change');");
        if let Some(field) = invalid_field {
            changed[field] = json!("fixture-secret-value");
        } else {
            changed.as_object_mut().unwrap().remove("call_id");
            changed.as_object_mut().unwrap().remove("name");
        }
        response(
            &mut decoder,
            &hub,
            id,
            vec![
                json!({"type":"response.created","response":{"id":"resp"}}),
                json!({"type":"response.output_item.done","item":item}),
                json!({"type":"response.output_item.done","item":changed}),
            ],
        )
        .await;
        let snapshot = hub.snapshot();
        assert!(snapshot.tools()[0].identity_conflict);
        assert_eq!(snapshot.tools()[0].arguments, "text('original');");
        assert!(
            !serde_json::to_string(&snapshot)
                .unwrap()
                .contains("fixture-secret-value")
        );
    }
}
