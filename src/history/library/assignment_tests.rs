use super::*;
fn parsed(payload: Value) -> Entry {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("record.jsonl"), "").unwrap();
    let source = Source::Native {
        id: "test".into(),
        codex_home: dir.path().into(),
    };
    let root = BlobRoot::open(dir.path()).unwrap();
    let mut file = NativeFile::open(&root, "record.jsonl".into(), &source).unwrap();
    file.parse(
        &serde_json::to_vec(&json!({"type":"session_meta","payload":payload})).unwrap(),
        &source,
        &[0; 32],
    );
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
