use super::*;

const THREAD: &str = "11111111-1111-4111-8111-111111111111";

fn command() -> Value {
    json!({"id": 7, "method": "item/commandExecution/requestApproval", "params": {
        "threadId": THREAD, "turnId": "synthetic-turn", "itemId": "synthetic-item",
        "command": "printf synthetic", "reason": "Run synthetic check",
        "cwd": "/private/synthetic-cwd", "commandActions": [{"type": "unknown-private-action"}],
        "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"]
    }})
}

fn question() -> Value {
    json!({"id": "question-rpc", "method": "item/tool/requestUserInput", "params": {
        "threadId": THREAD, "turnId": "synthetic-turn", "itemId": "synthetic-item",
        "isBlocking": true, "autoResolutionMs": null,
        "questions": [{"id": "original-question", "header": "Choice", "question": "Continue?",
            "isOther": false, "isSecret": false,
            "options": [{"label": "Yes (Recommended)", "description": "Continue synthetic task"},
                {"label": "No", "description": "Keep current state"}]}]
    }})
}

fn started() -> Value {
    json!({"method": "item/started", "params": {
        "threadId": THREAD, "turnId": "synthetic-turn", "startedAtMs": 123,
        "item": {"type": "fileChange", "id": "synthetic-item", "status": "inProgress",
            "changes": [{"path": "/synthetic/new.txt", "kind": {"type": "add"},
                "diff": "PRIVATE_DIFF_MUST_NOT_LEAVE"}]}
    }})
}

fn file_request() -> Value {
    json!({"id": "file-rpc", "method": "item/fileChange/requestApproval", "params": {
        "threadId": THREAD, "turnId": "synthetic-turn", "itemId": "synthetic-item",
        "grantRoot": null, "reason": "Synthetic change"
    }})
}

fn supported(value: &Value, scope: Option<&FileScope>) -> Request {
    match decode_request(value, scope).expect("synthetic request should decode") {
        Decode::Supported(request) => request,
        _ => panic!("expected supported synthetic request"),
    }
}

fn response(request: &Request, action: Action, selected: &[(usize, usize)]) -> Value {
    request
        .response(&action, selected)
        .expect("valid selection")
        .expect("complete selection")
        .envelope(&request.id)
}

#[test]
fn rpc_ids_keep_numeric_and_string_namespaces_distinct() {
    let number = RpcId::parse(&json!(7)).unwrap();
    let text = RpcId::parse(&json!("7")).unwrap();
    assert!(number != text);
    assert_eq!(number.to_value(), json!(7));
    assert_eq!(text.to_value(), json!("7"));
    for value in [
        Value::Null,
        json!(true),
        json!(7.5),
        json!(7.0),
        json!(u64::MAX),
        json!(""),
        json!("x\n"),
        json!("x".repeat(129)),
        json!([]),
    ] {
        assert!(RpcId::parse(&value).is_err());
    }
    assert!(RpcId::parse(&json!(i64::MIN)).is_ok());
}

#[test]
fn ordinary_command_has_only_typed_once_and_decline_and_no_cwd() {
    let request = supported(&command(), None);
    assert_eq!(request.actions(), vec![Action::Accept, Action::Decline]);
    assert_eq!(
        response(&request, Action::Accept, &[]),
        json!({"id": 7, "result": {"decision": "accept"}})
    );
    assert_eq!(
        response(&request, Action::Decline, &[]),
        json!({"id": 7, "result": {"decision": "decline"}})
    );
    let text = request.text();
    assert!(text.contains("printf synthetic") && text.contains("Run synthetic check"));
    for private in [
        "synthetic-cwd",
        "unknown-private-action",
        THREAD,
        "synthetic-item",
        "synthetic-turn",
    ] {
        assert!(!text.contains(private));
    }
    assert!(
        request
            .response(
                &Action::Choose {
                    question: 0,
                    option: 0
                },
                &[]
            )
            .is_err()
    );
    assert!(request.response(&Action::Accept, &[(0, 0)]).is_err());
}

#[test]
fn command_environment_allows_only_reserved_builtin_local() {
    for environment in [Value::Null, json!("local")] {
        let mut value = command();
        value["params"]["environmentId"] = environment;
        let request = supported(&value, None);
        assert_eq!(request.actions(), vec![Action::Accept, Action::Decline]);
        assert!(!request.text().contains("environmentId"));
    }
    for environment in [
        json!("remote"),
        json!("local-custom"),
        json!("LOCAL"),
        json!(""),
        json!({"id":"local"}),
    ] {
        let mut value = command();
        value["params"]["environmentId"] = environment;
        assert!(matches!(
            decode_request(&value, None),
            Ok(Decode::Unsupported)
        ));
    }
}

#[test]
fn command_decisions_are_intersected_and_never_widened() {
    for (decisions, expected) in [
        (json!(["accept"]), Action::Accept),
        (json!(["decline", "cancel"]), Action::Decline),
    ] {
        let mut value = command();
        value["params"]["availableDecisions"] = decisions;
        let request = supported(&value, None);
        assert_eq!(request.actions(), vec![expected]);
        let absent = if expected == Action::Accept {
            Action::Decline
        } else {
            Action::Accept
        };
        assert!(request.response(&absent, &[]).is_err());
    }
    for decisions in [
        json!([]),
        json!(["cancel", "acceptForSession"]),
        json!(["accept", "futureDecision"]),
    ] {
        let mut value = command();
        value["params"]["availableDecisions"] = decisions;
        assert!(matches!(
            decode_request(&value, None),
            Ok(Decode::Unsupported)
        ));
    }
    let mut duplicate = command();
    duplicate["params"]["availableDecisions"] = json!(["accept", "accept"]);
    assert!(matches!(
        decode_request(&duplicate, None),
        Err(Invalid::Duplicate)
    ));
    for value in [Value::Null, json!(["accept", "decline"])] {
        let mut request = command();
        request["params"]["availableDecisions"] = value;
        assert_eq!(supported(&request, None).actions().len(), 2);
    }
}

#[test]
fn known_policy_alternatives_are_validated_but_never_exported_or_selected() {
    let mut value = command();
    value["params"]["proposedExecpolicyAmendment"] = json!(["PRIVATE_POLICY"]);
    value["params"]["proposedNetworkPolicyAmendments"] =
        json!([{"host": "private.invalid", "action": "deny"}]);
    value["params"]["availableDecisions"] = json!(["accept", "decline",
        {"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["PRIVATE_POLICY"]}},
        {"applyNetworkPolicyAmendment": {"network_policy_amendment": {"host": "private.invalid", "action": "deny"}}}]);
    let request = supported(&value, None);
    assert!(
        !request.text().contains("PRIVATE_POLICY") && !request.text().contains("private.invalid")
    );
    assert_eq!(
        response(&request, Action::Accept, &[])["result"],
        json!({"decision": "accept"})
    );
    value["params"]["proposedNetworkPolicyAmendments"][0]["action"] = json!("future-action");
    assert!(decode_request(&value, None).is_err());
}

#[test]
fn broader_permissions_unknown_kind_and_malformed_command_fail_closed() {
    for (field, content) in [
        ("networkApprovalContext", json!({"host": "private.invalid"})),
        ("additionalPermissions", json!({})),
        ("kind", json!("writeStdin")),
        ("kind", json!("future")),
        ("environmentId", json!("remote-environment")),
    ] {
        let mut value = command();
        value["params"][field] = content;
        assert!(matches!(
            decode_request(&value, None),
            Ok(Decode::Unsupported)
        ));
    }
    for (field, content) in [
        ("command", json!("x".repeat(2401))),
        ("reason", json!("x".repeat(513))),
        ("command", json!("echo \u{1b}[2J")),
        ("command", json!("x\u{202e}y")),
        ("command", Value::Null),
        ("stdout", json!("PRIVATE_STDOUT")),
        ("env", json!({"PRIVATE": "VALUE"})),
    ] {
        let mut value = command();
        value["params"][field] = content;
        assert!(decode_request(&value, None).is_err());
    }
}

#[test]
fn file_scope_is_complete_bounded_metadata_without_diff() {
    let scope = FileScope::from_started(&started()).unwrap().unwrap();
    let request = supported(&file_request(), Some(&scope));
    assert!(request.text().contains("Add: /synthetic/new.txt"));
    assert!(!request.text().contains("PRIVATE_DIFF"));
    assert_eq!(
        response(&request, Action::Accept, &[]),
        json!({"id": "file-rpc", "result": {"decision": "accept"}})
    );
    let mut changed_diff = started();
    changed_diff["params"]["item"]["changes"][0]["diff"] = json!("another diff".repeat(10000));
    assert!(FileScope::from_started(&changed_diff).unwrap().unwrap() == scope);
}

#[test]
fn file_scope_supports_distinct_add_delete_update_and_move_metadata() {
    let mut value = started();
    value["params"]["item"]["changes"] = json!([
        {"path": "/synthetic/add", "kind": {"type": "add"}, "diff": "private"},
        {"path": "/synthetic/delete", "kind": {"type": "delete"}, "diff": "private"},
        {"path": "/synthetic/update", "kind": {"type": "update", "move_path": null}, "diff": "private"},
        {"path": "/synthetic/source", "kind": {"type": "update", "move_path": "/synthetic/target"}, "diff": "private"}
    ]);
    let scope = FileScope::from_started(&value).unwrap().unwrap();
    let text = supported(&file_request(), Some(&scope)).text();
    for expected in [
        "Add:",
        "Delete:",
        "Update:",
        "Update and move: /synthetic/source → /synthetic/target",
    ] {
        assert!(text.contains(expected));
    }
    value["params"]["item"]["changes"][3]["kind"]["move_path"] = json!("/synthetic/other-target");
    assert!(FileScope::from_started(&value).unwrap().unwrap() != scope);
}

#[test]
fn missing_foreign_preview_or_session_file_scope_cannot_authorize() {
    assert!(matches!(
        decode_request(&file_request(), None),
        Ok(Decode::Unsupported)
    ));
    let scope = FileScope::from_started(&started()).unwrap().unwrap();
    for (field, content) in [
        ("threadId", "22222222-2222-4222-8222-222222222222"),
        ("turnId", "foreign-turn"),
        ("itemId", "foreign-item"),
    ] {
        let mut value = file_request();
        value["params"][field] = json!(content);
        assert!(matches!(
            decode_request(&value, Some(&scope)),
            Err(Invalid::Scope)
        ));
    }
    let mut grant = file_request();
    grant["params"]["grantRoot"] = json!("/synthetic");
    assert!(matches!(
        decode_request(&grant, Some(&scope)),
        Ok(Decode::Unsupported)
    ));
    let mut preview = started();
    preview["method"] = json!("item/fileChange/patchUpdated");
    assert!(FileScope::from_started(&preview).unwrap().is_none());
    preview["method"] = json!("item/completed");
    assert!(FileScope::from_started(&preview).unwrap().is_none());
}

#[test]
fn file_scope_rejects_unknown_duplicate_alias_and_conflicting_paths() {
    for changes in [
        json!([{ "path": "/synthetic/a", "kind": {"type": "future"}, "diff": "" }]),
        json!([{ "path": "/synthetic/a", "kind": {"type": "add"}, "diff": "" }, { "path": "/synthetic/a", "kind": {"type": "delete"}, "diff": "" }]),
        json!([{ "path": "/synthetic/a", "kind": {"type": "update", "move_path": "/synthetic/b"}, "diff": "" }, { "path": "/synthetic/b", "kind": {"type": "add"}, "diff": "" }]),
        json!([{ "path": "/synthetic/a", "kind": {"type": "update", "move_path": "/synthetic/a"}, "diff": "" }]),
        json!([{ "path": "/synthetic/../a", "kind": {"type": "add"}, "diff": "" }]),
        json!([{ "path": "/synthetic/a\nDelete: other", "kind": {"type": "add"}, "diff": "" }]),
        json!([{ "path": "a".repeat(513), "kind": {"type": "add"}, "diff": "" }]),
        json!([]),
    ] {
        let mut value = started();
        value["params"]["item"]["changes"] = changes;
        assert!(FileScope::from_started(&value).is_err());
    }
    let mut value = started();
    value["params"]["item"]["changes"] = json!(
        (0..9)
            .map(
                |i| json!({"path": format!("/synthetic/{i}"), "kind": {"type": "add"}, "diff": ""})
            )
            .collect::<Vec<_>>()
    );
    assert!(matches!(
        FileScope::from_started(&value),
        Err(Invalid::Limit)
    ));
}

#[test]
fn questions_answer_original_ids_and_exact_labels_without_partial_results() {
    let mut value = question();
    let mut second = value["params"]["questions"][0].clone();
    second["id"] = json!("second-original-id");
    value["params"]["questions"]
        .as_array_mut()
        .unwrap()
        .push(second);
    let request = supported(&value, None);
    assert_eq!(request.actions().len(), 4);
    assert!(
        request
            .response(
                &Action::Choose {
                    question: 0,
                    option: 0
                },
                &[]
            )
            .unwrap()
            .is_none()
    );
    let envelope = response(
        &request,
        Action::Choose {
            question: 1,
            option: 1,
        },
        &[(0, 0)],
    );
    assert_eq!(
        envelope,
        json!({"id": "question-rpc", "result": {"answers": {
            "original-question": {"answers": ["Yes (Recommended)"]},
            "second-original-id": {"answers": ["No"]}
        }}})
    );
    assert!(!request.text().contains("original-question"));
    assert!(request.text().contains("1.1 Yes (Recommended)"));
    for (action, prior) in [
        (Action::Accept, vec![]),
        (
            Action::Choose {
                question: 2,
                option: 0,
            },
            vec![],
        ),
        (
            Action::Choose {
                question: 0,
                option: 2,
            },
            vec![],
        ),
        (
            Action::Choose {
                question: 0,
                option: 0,
            },
            vec![(0, 1)],
        ),
        (
            Action::Choose {
                question: 0,
                option: 0,
            },
            vec![(9, 0)],
        ),
    ] {
        assert!(request.response(&action, &prior).is_err());
    }
}

#[test]
fn questions_reject_secret_free_text_unknown_fields_and_malformed_values() {
    for (field, content) in [
        ("isSecret", json!(true)),
        ("isOther", json!(true)),
        ("options", Value::Null),
        ("options", json!([])),
    ] {
        let mut value = question();
        value["params"]["questions"][0][field] = content;
        assert!(matches!(
            decode_request(&value, None),
            Ok(Decode::Unsupported)
        ));
    }
    for (field, content) in [
        ("isSecret", json!("false")),
        ("isOther", Value::Null),
        ("future", json!(true)),
        ("id", json!("id\n")),
        ("question", json!("x".repeat(513))),
    ] {
        let mut value = question();
        value["params"]["questions"][0][field] = content;
        assert!(decode_request(&value, None).is_err());
    }
    let mut option = question();
    option["params"]["questions"][0]["options"][0]["future"] = json!(true);
    assert!(decode_request(&option, None).is_err());
    let mut unknown_params = question();
    unknown_params["params"]["future"] = json!(true);
    assert!(decode_request(&unknown_params, None).is_err());
}

#[test]
fn duplicate_question_ids_labels_and_oversized_sets_fail_closed() {
    let mut duplicate_id = question();
    let copy = duplicate_id["params"]["questions"][0].clone();
    duplicate_id["params"]["questions"]
        .as_array_mut()
        .unwrap()
        .push(copy);
    assert!(matches!(
        decode_request(&duplicate_id, None),
        Err(Invalid::Duplicate)
    ));
    let mut duplicate_label = question();
    duplicate_label["params"]["questions"][0]["options"][1]["label"] = json!("Yes (Recommended)");
    assert!(matches!(
        decode_request(&duplicate_label, None),
        Err(Invalid::Duplicate)
    ));
    let mut large = question();
    let sample = large["params"]["questions"][0].clone();
    large["params"]["questions"] = json!(vec![sample; 4]);
    assert!(matches!(decode_request(&large, None), Err(Invalid::Limit)));
    let mut large = question();
    let sample = large["params"]["questions"][0]["options"][0].clone();
    large["params"]["questions"][0]["options"] = json!(vec![sample; 5]);
    assert!(matches!(decode_request(&large, None), Err(Invalid::Limit)));
}

#[test]
fn telegram_limit_counts_utf16_and_refuses_incomplete_scope_or_questions() {
    let mut unicode = command();
    unicode["params"]["command"] = json!("😀".repeat(1200));
    assert!(supported(&unicode, None).text().encode_utf16().count() <= TEXT_LIMIT);
    unicode["params"]["command"] = json!("😀".repeat(1201));
    assert!(matches!(
        decode_request(&unicode, None),
        Err(Invalid::Limit)
    ));
    let mut large = question();
    let mut questions = Vec::new();
    for i in 0..3 {
        let mut q = large["params"]["questions"][0].clone();
        q["id"] = json!(format!("q{i}"));
        q["question"] = json!("q".repeat(512));
        q["options"] = json!((0..4).map(|j| json!({"label": format!("{j}{}", "l".repeat(127)), "description": "d".repeat(160)})).collect::<Vec<_>>());
        questions.push(q);
    }
    large["params"]["questions"] = json!(questions);
    assert!(matches!(decode_request(&large, None), Err(Invalid::Limit)));
    let mut large = started();
    large["params"]["item"]["changes"] = json!((0..8).map(|i| json!({"path": format!("/{i}{}", "p".repeat(510)), "kind": {"type": "add"}, "diff": "private"})).collect::<Vec<_>>());
    assert!(matches!(
        FileScope::from_started(&large),
        Err(Invalid::Limit)
    ));
}

#[test]
fn unrelated_notifications_are_ignored_and_unknown_requests_unsupported() {
    for value in [
        json!({"method": "item/started", "params": {}}),
        json!({"id": 1, "result": {}}),
    ] {
        assert!(matches!(decode_request(&value, None), Ok(Decode::Ignored)));
    }
    assert!(matches!(
        decode_request(
            &json!({"id": 1, "method": "future/private", "params": {"secret": "private"}}),
            None
        ),
        Ok(Decode::Unsupported)
    ));
    let mut value = command();
    value["result"] = json!({});
    assert!(decode_request(&value, None).is_err());
}
