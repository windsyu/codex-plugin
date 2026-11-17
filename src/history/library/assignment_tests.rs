use super::*;
#[test]
fn human_title_and_preview_skip_complete_injected_context() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("record.jsonl"), "").unwrap();
    let source = Source::Native {
        id: "test".into(),
        codex_home: dir.path().into(),
    };
    let root = BlobRoot::open(dir.path()).unwrap();
    let mut file = NativeFile::open(&root, "record.jsonl".into(), &source).unwrap();
    for (kind, message) in [
        (
            "user_message",
            "# AGENTS.md instructions for /synthetic/project\n\n<INSTRUCTIONS>\nBuild carefully.\n</INSTRUCTIONS>",
        ),
        ("user_message", "修复历史页面的会话标题"),
        ("agent_message", "已定位到标题提取规则"),
        ("token_count", "internal usage"),
    ] {
        file.parse(
            &serde_json::to_vec(
                &json!({"type":"event_msg","payload":{"type":kind,"message":message}}),
            )
            .unwrap(),
            &source,
            &[0; 32],
        );
    }
    assert_eq!(file.document.entry.title, "修复历史页面的会话标题");
    assert_eq!(
        json!(file.document.entry)["preview"],
        json!({"text":"已定位到标题提取规则","role":"assistant"})
    );
    assert!(
        file.document.records[0]["raw"]["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("<INSTRUCTIONS>")
    );
}
fn parsed(payload: Value) -> Entry {
    parsed_sequence(&[payload])
}

#[test]
fn guardian_metadata_excludes_review_exchange_but_not_user_discussion() {
    for (source_meta, is_review) in [
        (json!({"subagent":{"other":"guardian"}}), true),
        (json!("cli"), false),
        (json!({"subagent":{"other":"future"}}), false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("record.jsonl"), "").unwrap();
        let source = Source::Native {
            id: "test".into(),
            codex_home: dir.path().into(),
        };
        let root = BlobRoot::open(dir.path()).unwrap();
        let mut file = NativeFile::open(&root, "record.jsonl".into(), &source).unwrap();
        let request = "The following is the Codex agent history whose request action you are assessing. Synthetic transcript.";
        let answer = r#"{"risk_level":"low","user_authorization":"high","outcome":"allow"}"#;
        for raw in [
            json!({"type":"session_meta","payload":{"source":source_meta}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":request}]}}),
            json!({"type":"event_msg","payload":{"type":"agent_message","message":answer}}),
        ] {
            file.parse(&serde_json::to_vec(&raw).unwrap(), &source, &[0; 32]);
        }
        if is_review {
            assert_eq!(file.document.entry.title, "未命名会话");
            assert!(file.document.entry.preview.is_none());
        } else {
            assert_eq!(file.document.entry.title, request);
            assert_eq!(file.document.entry.preview.as_ref().unwrap().text, answer);
        }
        for record in &file.document.records[1..] {
            assert_eq!(
                presentation::normalize(record)["presentation"],
                if is_review {
                    "activity"
                } else {
                    "conversation"
                }
            );
            assert!(record.get("raw").is_some());
        }
        assert_eq!(
            file.document.records[1]["raw"]["payload"]["content"][0]["text"],
            request
        );
    }
}

#[test]
fn preview_advances_after_native_body_budget_and_long_context_does_not_become_title() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("record.jsonl"), "").unwrap();
    let source = Source::Native {
        id: "test".into(),
        codex_home: dir.path().into(),
    };
    let root = BlobRoot::open(dir.path()).unwrap();
    let mut file = NativeFile::open(&root, "record.jsonl".into(), &source).unwrap();
    let long = format!(
        "# AGENTS.md instructions for /synthetic\n<INSTRUCTIONS>{}</INSTRUCTIONS>\n真正的请求",
        "context ".repeat(10000)
    );
    file.parse(&serde_json::to_vec(&json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":long}]}})).unwrap(), &source, &[0;32]);
    assert_eq!(file.document.entry.title, "真正的请求");
    assert_eq!(
        presentation::normalize(&file.document.records[0])["text"],
        "真正的请求"
    );
    file.bytes = BODY;
    let count = file.document.records.len();
    file.parse(&serde_json::to_vec(&json!({"type":"event_msg","payload":{"type":"agent_message","message":"缓存之外的最新回答"}})).unwrap(), &source, &[0;32]);
    assert_eq!(file.document.records.len(), count);
    assert_eq!(
        file.document.entry.preview.unwrap().text,
        "缓存之外的最新回答"
    );
}
fn parsed_sequence(payloads: &[Value]) -> Entry {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("record.jsonl"), "").unwrap();
    let source = Source::Native {
        id: "test".into(),
        codex_home: dir.path().into(),
    };
    let root = BlobRoot::open(dir.path()).unwrap();
    let mut file = NativeFile::open(&root, "record.jsonl".into(), &source).unwrap();
    for payload in payloads {
        file.parse(
            &serde_json::to_vec(&json!({"type":"session_meta","payload":payload})).unwrap(),
            &source,
            &[0; 32],
        );
    }
    file.document.entry
}
#[test]
fn desktop_execution_directory_is_not_a_project() {
    let cwd = "/Users/demo/Documents/Codex/2026-09-23/new-chat";
    let entry = parsed(json!({"cwd":cwd,"originator":"Codex Desktop"}));
    assert!(entry.project_id.is_none());
    let ordinary = parsed(json!({"cwd":cwd,"originator":"codex-tui"}));
    assert!(ordinary.project_id.is_some());
}
#[test]
fn top_level_parent_is_preserved_and_conflicts_do_not_link() {
    let parent = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let entry = parsed(json!({"parent_thread_id":parent,"agent_nickname":"helper"}));
    assert_eq!(entry.parent_thread_id.as_deref(), Some(parent));
    assert!(entry.is_subagent);
    let conflict = parsed(
        json!({"parent_thread_id":parent,"source":{"subagent":{"thread_spawn":{"parent_thread_id":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"}}}}),
    );
    assert!(conflict.parent_entry_id.is_none());
}

#[test]
fn classification_preserves_explicit_and_non_git_directories() {
    for cwd in [
        "/tmp/new-chat",
        "/tmp/Documents/Codex/2026-99-99/chat",
        "/tmp/Documents/Codex/2026-09-23/chat/child",
    ] {
        let entry = parsed(json!({"cwd":cwd,"originator":"Codex Desktop"}));
        assert!(entry.project_id.is_some(), "{cwd}");
        assert_eq!(entry.recorded_cwd.as_deref(), Some(cwd));
        assert_eq!(entry.project_basis, "cwd_inferred");
    }
    let entry = parsed(
        json!({"cwd":"C:\\Users\\demo\\Documents\\Codex\\2026-09-23\\chat","originator":"codex_work_desktop"}),
    );
    assert!(entry.project_id.is_none());
    assert_eq!(entry.project_basis, "desktop_generated");
    let source = Source::Workbench {
        id: "wb".into(),
        data_directory: "/tmp/test".into(),
    };
    let mut explicit = Entry::new(&source, "test");
    explicit.path("/tmp/Documents/Codex/2026-09-23/chat");
    assert!(explicit.project_id.is_some());
    assert!(
        parsed(json!({"cwd":"/tmp/a/same"})).project_id
            != parsed(json!({"cwd":"/tmp/b/same"})).project_id
    );
}

#[test]
fn legacy_project_identity_is_independent_of_cwd_and_field_issues() {
    let source = Source::Observer {
        id: "old".into(),
        database: "/tmp/test.db".into(),
        blob_directory: None,
        native_home: None,
    };
    let mut entry = Entry::new(&source, "test");
    entry.legacy_path(&json!({"cwd":"/tmp/execution","projectKey":null}), &[]);
    assert_eq!(entry.recorded_cwd.as_deref(), Some("/tmp/execution"));
    assert!(entry.project_id.is_none());
    assert_eq!(entry.project_basis, "legacy_recorded");
    entry.legacy_path(
        &json!({"cwd":"/tmp/execution","projectKey":"/tmp/project"}),
        &[],
    );
    assert_eq!(entry.project_path.as_deref(), Some("/tmp/project"));
    assert_eq!(entry.recorded_cwd.as_deref(), Some("/tmp/execution"));
    entry.legacy_path(
        &json!({"cwd":"/tmp/execution","projectKey":null}),
        &["field_too_large:projectKey".into()],
    );
    assert_eq!(entry.project_basis, "unknown");
    assert!(
        entry
            .coverage
            .reasons
            .contains(&"field_too_large:projectKey".into())
    );
    for fields in [
        json!({"cwd":"/tmp/execution"}),
        json!({"projectKey":42}),
        json!({"projectKey":"relative"}),
    ] {
        entry.legacy_path(&fields, &[]);
        assert!(entry.project_id.is_none());
        assert_eq!(entry.project_basis, "unknown");
    }
}

#[test]
fn parent_shapes_require_reliable_identity() {
    let parent = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    for value in [
        json!({"source":{"subagent":{"thread_spawn":{"parent_thread_id":parent,"agent_role":"worker"}}}}),
        json!({"parent_thread_id":parent,"source":{"subagent":{"thread_spawn":{"parent_thread_id":parent}}}}),
    ] {
        let entry = parsed(value);
        assert_eq!(entry.parent_thread_id.as_deref(), Some(parent));
        assert!(entry.parent_entry_id.is_some());
        assert!(entry.project_id.is_none());
    }
    let invalid = parsed(json!({"parent_thread_id":"not-a-thread"}));
    assert!(invalid.parent_entry_id.is_none());
    assert!(
        invalid
            .coverage
            .reasons
            .contains(&"invalid_parent_identity".into())
    );
}

#[test]
fn conversation_kind_requires_closed_reliable_evidence() {
    let parent = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    for (payload, kind, basis) in [
        (json!({"source":"cli"}), "ordinary", "native_source_cli_v1"),
        (
            json!({"source":"cli","model":"guardian","agent_role":"reviewer"}),
            "ordinary",
            "native_source_cli_v1",
        ),
        (json!({}), "unknown", "source_missing_v1"),
        (
            json!({"source":{"subagent":{"other":"guardian"}}}),
            "background_review",
            "native_source_guardian_v1",
        ),
        (
            json!({"source":{"subagent":{"other":"guardian"}},"parent_thread_id":"invalid"}),
            "unknown",
            "metadata_invalid_v1",
        ),
        (
            json!({"source":{"subagent":{"other":"future"}}}),
            "unknown",
            "source_unknown_v1",
        ),
        (
            json!({"source":{"subagent":{"other":"guardian","thread_spawn":{}}}}),
            "unknown",
            "source_unknown_v1",
        ),
        (
            json!({"source":{"subagent":{"other":"guardian"},"future":true}}),
            "unknown",
            "source_unknown_v1",
        ),
        (
            json!({"parent_thread_id":parent}),
            "subagent",
            "native_parent_thread_v1",
        ),
        (
            json!({"source":"sub_agent","parent_thread_id":parent}),
            "subagent",
            "native_parent_thread_v1",
        ),
        (
            json!({"source":{"subagent":{"thread_spawn":{}}}}),
            "subagent",
            "native_source_thread_spawn_v1",
        ),
        (
            json!({"source":"cli","parent_thread_id":parent}),
            "unknown",
            "metadata_conflict_v1",
        ),
        (
            json!({"source":"future","parent_thread_id":parent}),
            "unknown",
            "source_unknown_v1",
        ),
        (json!({"source":{}}), "unknown", "source_unknown_v1"),
        (json!({"source":42}), "unknown", "metadata_invalid_v1"),
        (json!({"source":[]}), "unknown", "metadata_invalid_v1"),
        (
            json!({"source":{"subagent":{"thread_spawn":null}}}),
            "unknown",
            "metadata_invalid_v1",
        ),
        (
            json!({"source":{"subagent":{"thread_spawn":[]}}}),
            "unknown",
            "metadata_invalid_v1",
        ),
        (
            json!({"source":{"subagent":{}}}),
            "unknown",
            "source_unknown_v1",
        ),
        (
            json!({"source":{"subagent":{"guardian_review":{}}}}),
            "unknown",
            "source_unknown_v1",
        ),
        (
            json!({"source":"sub_agent"}),
            "unknown",
            "source_missing_v1",
        ),
        (
            json!({"parent_thread_id":"bad"}),
            "unknown",
            "metadata_invalid_v1",
        ),
        (
            json!({"parent_thread_id":"00000000-0000-0000-0000-000000000000"}),
            "unknown",
            "metadata_invalid_v1",
        ),
        (
            json!({"source":{"subagent":{"thread_spawn":{"parent_thread_id":parent}}},"parent_thread_id":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"}),
            "unknown",
            "metadata_conflict_v1",
        ),
    ] {
        let entry = serde_json::to_value(parsed(payload.clone())).unwrap();
        assert_eq!(entry["conversationKind"], kind, "{payload}");
        assert_eq!(entry["kindBasis"], basis, "{payload}");
    }
}

#[test]
fn repeated_session_metadata_cannot_wash_away_kind_or_identity_conflicts() {
    let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let other = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    for rows in [
        vec![
            json!({"source":"cli"}),
            json!({"parent_thread_id":id}),
            json!({"source":"cli"}),
        ],
        vec![json!({"source":"future"}), json!({"source":"cli"})],
        vec![
            json!({"parent_thread_id":"bad"}),
            json!({"parent_thread_id":id}),
        ],
        vec![
            json!({"id":id,"source":"cli"}),
            json!({"id":other,"source":"cli"}),
            json!({"id":id,"source":"cli"}),
        ],
    ] {
        let entry = parsed_sequence(&rows);
        assert_eq!(entry.conversation_kind, "unknown");
        assert_eq!(entry.kind_basis, "metadata_conflict_v1");
        assert!(
            entry
                .coverage
                .reasons
                .iter()
                .any(|s| s.starts_with("conflicting_"))
        );
    }
    let entry = parsed_sequence(&[json!({"source":"cli"}), json!({"source":"cli"})]);
    assert_eq!(entry.conversation_kind, "ordinary");
}

#[test]
fn declared_fork_metadata_keeps_the_child_identity_and_project() {
    let child = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let parent = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let own = json!({"id":child,"forked_from_id":parent,"parent_thread_id":parent,
        "source":{"subagent":{"thread_spawn":{"parent_thread_id":parent}}},
        "cwd":"/synthetic/child","timestamp":"2026-11-17T10:00:00Z"});
    let inherited = json!({"id":parent,"source":"cli","cwd":"/synthetic/parent",
        "timestamp":"2026-11-16T10:00:00Z"});
    let entry = parsed_sequence(&[own.clone(), inherited.clone()]);
    assert_eq!(entry.native_thread_id.as_deref(), Some(child));
    assert_eq!(entry.parent_thread_id.as_deref(), Some(parent));
    assert_eq!(entry.recorded_cwd.as_deref(), Some("/synthetic/child"));
    assert_eq!(entry.project_path.as_deref(), Some("/synthetic/child"));
    assert_eq!(
        entry.recorded_at.as_deref(),
        Some("2026-11-17T10:00:00.000Z")
    );
    assert_eq!(entry.conversation_kind, "subagent");
    assert_eq!(entry.kind_basis, "native_source_thread_spawn_v1");
    assert!(entry.capabilities.resume);
    assert!(entry.coverage.reasons.is_empty());

    let dir = tempfile::tempdir().unwrap();
    let source = Source::Native {
        id: "test".into(),
        codex_home: dir.path().into(),
    };
    let rows = [own, inherited].map(|payload| json!({"type":"session_meta","payload":payload}));
    let text = rows
        .iter()
        .map(|raw| format!("{raw}\n"))
        .collect::<String>();
    std::fs::write(dir.path().join("record.jsonl"), &text).unwrap();
    let root = BlobRoot::open(dir.path()).unwrap();
    let mut file = NativeFile::open(&root, "record.jsonl".into(), &source).unwrap();
    while !file.step(&source, &root, &[0; 32]).unwrap() {}
    assert_eq!(file.document.entry.conversation_kind, "subagent");
    assert_eq!(file.document.records.len(), 2);
    assert_eq!(file.document.records[1]["kind"], "context");
    assert_eq!(file.document.records[1]["raw"], rows[1]);
    let inherited_window = window(
        &source,
        &file.document.locator,
        &file.document.entry,
        &json!({"offset":format!("{}\n", rows[0]).len()}),
        1,
        true,
        &[0; 32],
    )
    .unwrap();
    assert_eq!(inherited_window.records.len(), 1);
    assert_eq!(inherited_window.records[0].0["kind"], "context");
    assert_eq!(inherited_window.records[0].0["raw"], rows[1]);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("record.jsonl")).unwrap(),
        text
    );

    let unassigned = parsed_sequence(&[
        json!({"id":child,"source":"cli","forked_from_id":parent}),
        rows[1]["payload"].clone(),
    ]);
    assert!(unassigned.recorded_cwd.is_none());
    assert!(unassigned.project_id.is_none());
    assert_eq!(unassigned.conversation_kind, "ordinary");
}

#[test]
fn only_the_original_declared_fork_can_explain_foreign_metadata() {
    let child = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let parent = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let other = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    for fork in [Value::Null, json!("invalid"), json!(child), json!(other)] {
        let entry = parsed_sequence(&[
            json!({"id":child,"source":"sub_agent","parent_thread_id":parent,"forked_from_id":fork,"cwd":"/synthetic/child"}),
            json!({"id":parent,"source":"cli","cwd":"/synthetic/parent"}),
        ]);
        assert_eq!(entry.native_thread_id.as_deref(), Some(child));
        assert_eq!(entry.recorded_cwd.as_deref(), Some("/synthetic/child"));
        assert_eq!(entry.conversation_kind, "unknown");
        assert!(!entry.capabilities.resume);
        assert!(
            entry
                .coverage
                .reasons
                .contains(&"conflicting_session_identity".into())
        );
    }
    let entry = parsed_sequence(&[
        json!({"id":child,"source":"cli","cwd":"/synthetic/child"}),
        json!({"id":child,"source":"cli","forked_from_id":parent,"cwd":"/synthetic/child"}),
        json!({"id":parent,"source":"cli","cwd":"/synthetic/parent"}),
        json!({"id":child,"source":"cli","cwd":"/synthetic/child"}),
    ]);
    assert_eq!(entry.conversation_kind, "unknown");
    assert!(!entry.capabilities.resume);
    assert!(
        entry
            .coverage
            .reasons
            .contains(&"conflicting_session_identity".into())
    );
}

#[test]
fn unreliable_or_changed_metadata_cannot_replace_the_owner_directory() {
    let child = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let parent = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let own = json!({"id":child,"forked_from_id":parent,"source":"cli","cwd":"/synthetic/child"});
    for id in [
        Value::Null,
        json!("invalid"),
        json!(uuid::Uuid::nil()),
        json!(child),
    ] {
        let mut changed = own.clone();
        changed["id"] = id;
        changed["cwd"] = json!("/synthetic/parent");
        let entry = parsed_sequence(&[own.clone(), changed, own.clone()]);
        assert_eq!(entry.native_thread_id.as_deref(), Some(child));
        assert_eq!(entry.recorded_cwd.as_deref(), Some("/synthetic/child"));
        assert_eq!(entry.project_path.as_deref(), Some("/synthetic/child"));
        assert_eq!(entry.conversation_kind, "unknown");
        assert!(!entry.capabilities.resume);
        assert!(
            entry
                .coverage
                .reasons
                .iter()
                .any(|code| code.starts_with("conflicting_session_"))
        );
    }
    let unchanged = parsed_sequence(&[own.clone(), own]);
    assert!(unchanged.capabilities.resume);
    assert!(unchanged.coverage.reasons.is_empty());
}

#[test]
fn an_unreliable_initial_identity_remains_readable_but_cannot_resume() {
    let parent = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    for id in [Value::Null, json!("invalid"), json!(uuid::Uuid::nil())] {
        let first = json!({"id":id,"source":"cli","cwd":"/synthetic/child"});
        let no_identity = parsed(first.clone());
        assert!(no_identity.native_thread_id.is_none());
        assert!(!no_identity.capabilities.resume);
        assert!(no_identity.capabilities.read);
        let later = parsed_sequence(&[
            first,
            json!({"id":parent,"source":"cli","cwd":"/synthetic/parent"}),
        ]);
        assert!(!later.capabilities.resume);
        assert!(later.capabilities.read);
        for reliable in [
            json!({"id":parent,"source":"cli"}),
            json!({"id":parent,"source":"cli","cwd":null}),
        ] {
            let missing_cwd = parsed_sequence(&[
                json!({"id":id,"source":"cli","cwd":"/synthetic/unreliable"}),
                reliable,
            ]);
            assert!(missing_cwd.recorded_cwd.is_none());
            assert!(missing_cwd.project_id.is_none());
            assert!(missing_cwd.project_path.is_none());
            assert_eq!(missing_cwd.project_basis, "unknown");
            assert!(!missing_cwd.capabilities.resume);
        }
    }
}

#[test]
fn observer_kind_requires_reliable_parent_and_clean_related_fields() {
    let parent = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    for fields in [
        json!({"parentThreadId":parent}),
        json!({"parentThreadKey":"opaque-key"}),
    ] {
        assert_eq!(
            observer_conversation_kind(&fields, &[]),
            ("subagent", "observer_parent_relation_v1")
        );
        for issue in [
            "field_too_large:parentThreadId",
            "field_unavailable:parentThreadKey",
            "identity_conflict",
            "source_revision_changed",
        ] {
            assert_eq!(
                observer_conversation_kind(&fields, &[issue.into()]).0,
                "unknown"
            );
        }
        assert_eq!(
            observer_conversation_kind(&fields, &["field_too_large:agentNickname".into()]).0,
            "subagent"
        );
    }
    for fields in [
        json!({}),
        json!({"parentThreadId":"bad"}),
        json!({"parentThreadId":42,"parentThreadKey":"key"}),
        json!({"parentThreadKey":""}),
        json!({"parentThreadKey":true}),
    ] {
        assert_eq!(observer_conversation_kind(&fields, &[]).0, "unknown");
    }
    let source = Source::Workbench {
        id: "test".into(),
        data_directory: "/tmp/synthetic".into(),
    };
    let entry = Entry::new(&source, "run");
    assert_eq!(entry.conversation_kind, "unknown");
    assert_eq!(entry.kind_basis, "workbench_run_v1");
}

#[test]
fn r7_unassigned_reasons_preserve_evidence_instead_of_guessing_project_intent() {
    for (payload, expected) in [
        (json!({}), "missing_cwd"),
        (json!({"cwd":"relative"}), "invalid_metadata"),
        (
            json!({"cwd":"/Users/demo/Documents/Codex/2026-09-23/chat", "originator":"Codex Desktop"}),
            "desktop_generated",
        ),
        (
            json!({"parent_thread_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "source":{"subagent":{"thread_spawn":{"parent_thread_id":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"}}}}),
            "identity_conflict",
        ),
    ] {
        assert_eq!(unassigned_reason(&json!(parsed(payload))), Some(expected));
    }
    let source = Source::Observer {
        id: "legacy".into(),
        database: "/tmp/synthetic.db".into(),
        blob_directory: None,
        native_home: None,
    };
    let mut entry = Entry::new(&source, "thread");
    entry.legacy_path(
        &json!({"projectKey":null,"cwd":"/synthetic/execution"}),
        &[],
    );
    assert_eq!(unassigned_reason(&json!(entry)), Some("source_unassigned"));
    entry.project_basis = "unknown".into();
    assert_eq!(unassigned_reason(&json!(entry)), Some("unknown"));
    entry.path("/synthetic/execution");
    assert_eq!(unassigned_reason(&json!(entry)), None);
}
