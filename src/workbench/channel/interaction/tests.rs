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

fn response(request: &Request, action: Action, selected: &[Answer]) -> Value {
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
    assert!(
        request
            .response(
                &Action::Accept,
                &[supported(&question(), None).select(0, 0).unwrap()]
            )
            .is_err()
    );
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
        &[request.select(0, 0).unwrap()],
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
            vec![request.select(0, 1).unwrap()],
        ),
        (
            Action::Choose {
                question: 0,
                option: 0,
            },
            vec![{
                let mut answer = request.select(0, 0).unwrap();
                answer.question = 9;
                answer
            }],
        ),
    ] {
        assert!(request.response(&action, &prior).is_err());
    }
}

#[test]
fn questions_reject_secret_unknown_fields_and_malformed_values() {
    let mut secret = question();
    secret["params"]["questions"][0]["isSecret"] = json!(true);
    assert!(matches!(
        decode_request(&secret, None),
        Ok(Decode::Unsupported)
    ));
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

fn mixed_questions() -> Request {
    let mut value = question();
    let original = value["params"]["questions"][0].clone();
    let mut other = original.clone();
    other["id"] = json!("other-original");
    other["header"] = json!("Second");
    other["isOther"] = json!(true);
    let mut text = original.clone();
    text["id"] = json!("free-original");
    text["header"] = json!("Third");
    text["options"] = Value::Null;
    value["params"]["questions"] = json!([original, other, text]);
    supported(&value, None)
}

#[test]
fn no_options_and_other_questions_have_precise_answer_modes() {
    for options in [None, Some(Value::Null), Some(json!([]))] {
        for is_other in [false, true] {
            let mut value = question();
            let fields = value["params"]["questions"][0].as_object_mut().unwrap();
            fields.insert("isOther".into(), json!(is_other));
            if let Some(options) = &options {
                fields.insert("options".into(), options.clone());
            } else {
                fields.remove("options");
            }
            let request = supported(&value, None);
            assert!(request.allows_text(0));
            assert!(!request.allows_text(1));
            assert!(request.actions().is_empty());
            assert!(request.select(0, 0).is_err());
            let answer = request
                .answer_text(0, " custom answer\ncontinued ")
                .unwrap();
            assert_eq!(
                request
                    .answers_response(&[answer])
                    .unwrap()
                    .unwrap()
                    .envelope(&request.id)["result"],
                json!({"answers": {"original-question": {"answers": [" custom answer\ncontinued "]}}})
            );
        }
    }
    let mixed = mixed_questions();
    assert!(!mixed.allows_text(0));
    assert!(mixed.answer_text(0, "wrong mode").is_err());
    assert!(mixed.allows_text(1));
    assert!(mixed.select(1, 1).is_ok());
    assert!(mixed.answer_text(1, "Another option").is_ok());
    assert!(mixed.allows_text(2));
    let mut single_option = question();
    single_option["params"]["questions"][0]["options"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(matches!(
        decode_request(&single_option, None),
        Err(Invalid::Limit)
    ));
}

#[test]
fn mixed_answers_complete_once_per_question_with_exact_original_content() {
    let request = mixed_questions();
    let mut selected = Vec::new();
    assert_eq!(request.next_question(&selected).unwrap(), Some(0));
    assert!(request.answers_response(&selected).unwrap().is_none());
    // Arrival order does not change the original question association.
    selected.push(
        request
            .answer_text(2, "  自定义 😀\nsecond line  ")
            .unwrap(),
    );
    assert_eq!(request.next_question(&selected).unwrap(), Some(0));
    assert!(request.answers_response(&selected).unwrap().is_none());
    selected.push(request.select(0, 1).unwrap());
    assert_eq!(request.next_question(&selected).unwrap(), Some(1));
    assert!(request.answers_response(&selected).unwrap().is_none());
    let via_choice = response(
        &request,
        Action::Choose {
            question: 1,
            option: 0,
        },
        &selected,
    );
    assert_eq!(
        via_choice["result"]["answers"]["other-original"],
        json!({"answers": ["Yes (Recommended)"]})
    );
    selected.push(request.answer_text(1, "my other answer").unwrap());
    assert_eq!(request.next_question(&selected).unwrap(), None);
    let envelope = request
        .answers_response(&selected)
        .unwrap()
        .unwrap()
        .envelope(&request.id);
    assert_eq!(
        envelope,
        json!({"id": "question-rpc", "result": {"answers": {
            "original-question": {"answers": ["No"]},
            "other-original": {"answers": ["my other answer"]},
            "free-original": {"answers": ["  自定义 😀\nsecond line  "]}
        }}})
    );
    assert!(
        request
            .response(
                &Action::Choose {
                    question: 1,
                    option: 1
                },
                &selected
            )
            .is_err()
    );
    selected.push(request.select(1, 0).unwrap());
    assert!(request.answers_response(&selected).is_err());
    assert!(request.next_question(&selected).is_err());
}

#[test]
fn per_question_text_has_one_question_original_numbering_and_no_native_ids() {
    let request = mixed_questions();
    let text = request.question_text(1).unwrap();
    assert!(text.contains("2. Second") && text.contains("2.1 Yes (Recommended)"));
    assert!(text.contains("Reply directly to this message"));
    for excluded in [
        "1. Choice",
        "3. Third",
        "other-original",
        "question-rpc",
        THREAD,
        "synthetic-turn",
        "synthetic-item",
    ] {
        assert!(!text.contains(excluded));
    }
    assert!(!request.question_text(0).unwrap().contains("Reply directly"));
    assert!(request.question_text(3).is_err());
    assert!(request.text().encode_utf16().count() <= TEXT_LIMIT);
    let command = supported(&command(), None);
    assert!(command.question_text(0).is_err());
    assert!(!command.allows_text(0));
    assert!(command.next_question(&[]).is_err());
    assert!(command.answers_response(&[]).is_err());
    assert!(command.answer_text(0, "answer").is_err());
}

#[test]
fn free_text_preserves_content_and_enforces_unicode_control_and_length_limits() {
    let request = mixed_questions();
    for text in [
        "x".repeat(1024),
        "😀".repeat(512),
        "界".repeat(1024),
        "\nline\n".into(),
        " /start is literal input ".into(),
    ] {
        let answer = request.answer_text(2, &text).unwrap();
        assert!(request.next_question(&[answer]).is_ok());
    }
    for text in [
        String::new(),
        " \n ".into(),
        "x".repeat(1025),
        "😀".repeat(513),
        "界".repeat(1366),
    ] {
        assert!(matches!(request.answer_text(2, &text), Err(Invalid::Limit)));
    }
    for control in [
        '\t', '\r', '\0', '\u{1b}', '\u{7f}', '\u{85}', '\u{061c}', '\u{200e}', '\u{200f}',
        '\u{202a}', '\u{202e}', '\u{2066}', '\u{2069}',
    ] {
        assert!(matches!(
            request.answer_text(2, &format!("a{control}b")),
            Err(Invalid::Control)
        ));
    }
}

#[test]
fn selected_answers_are_revalidated_for_duplicates_forgery_and_changed_schema() {
    let request = mixed_questions();
    let original = request.answer_text(1, "custom").unwrap();
    for selected in [
        vec![original.clone(), original.clone()],
        vec![original.clone(), request.select(1, 0).unwrap()],
    ] {
        assert!(request.answers_response(&selected).is_err());
        assert!(request.next_question(&selected).is_err());
        assert!(
            request
                .response(
                    &Action::Choose {
                        question: 0,
                        option: 0
                    },
                    &selected
                )
                .is_err()
        );
    }
    let mut forged_index = original.clone();
    forged_index.question = 3;
    let mut forged_text = original.clone();
    forged_text.value = AnswerValue::Text("bad\ttext".into());
    let mut forged_option = original.clone();
    forged_option.value = AnswerValue::Option {
        index: 0,
        label: "unknown answer".into(),
    };
    for answer in [forged_index, forged_text, forged_option] {
        assert!(
            request
                .answers_response(std::slice::from_ref(&answer))
                .is_err()
        );
        assert!(request.next_question(&[answer]).is_err());
    }
    let mut changed = request.clone();
    if let Kind::Questions(questions) = &mut changed.kind {
        questions[1].allows_text = false;
    }
    assert!(changed.answers_response(&[original]).is_err());
    let answer = request.select(0, 0).unwrap();
    if let Kind::Questions(questions) = &mut changed.kind {
        questions[0].options[0].label = "changed".into();
    }
    assert!(changed.answers_response(&[answer]).is_err());
}

#[test]
fn answers_cannot_be_reused_across_request_or_question_identities() {
    let request = mixed_questions();
    let answer = request.answer_text(1, "private answer").unwrap();
    for field in ["rpc", "rpc-type", "thread", "turn", "item", "question"] {
        let mut foreign = request.clone();
        match field {
            "rpc" => foreign.id = RpcId::String("different-rpc".into()),
            "rpc-type" => foreign.id = RpcId::Number(1),
            "thread" => foreign.thread = Uuid::nil(),
            "turn" => foreign.turn = "different-turn".into(),
            "item" => foreign.item = "different-item".into(),
            "question" => {
                if let Kind::Questions(questions) = &mut foreign.kind {
                    questions[1].id = "different-question".into();
                }
            }
            _ => unreachable!(),
        }
        assert!(
            foreign
                .answers_response(std::slice::from_ref(&answer))
                .is_err()
        );
        assert!(
            foreign
                .next_question(std::slice::from_ref(&answer))
                .is_err()
        );
        assert!(
            foreign
                .response(
                    &Action::Choose {
                        question: 0,
                        option: 0
                    },
                    std::slice::from_ref(&answer)
                )
                .is_err()
        );
    }
    let mut numeric = request.clone();
    numeric.id = RpcId::Number(1);
    let numeric_answer = numeric.answer_text(1, "answer").unwrap();
    numeric.id = RpcId::String("1".into());
    assert!(numeric.answers_response(&[numeric_answer]).is_err());
}

#[test]
fn secret_question_anywhere_keeps_entire_mixed_request_local() {
    for position in 0..3 {
        let mut value = question();
        let original = value["params"]["questions"][0].clone();
        let mut questions = Vec::new();
        for index in 0..3 {
            let mut question = original.clone();
            question["id"] = json!(format!("q{index}"));
            question["options"] = Value::Null;
            question["isSecret"] = json!(index == position);
            questions.push(question);
        }
        value["params"]["questions"] = json!(questions);
        assert!(matches!(
            decode_request(&value, None),
            Ok(Decode::Unsupported)
        ));
    }
}
