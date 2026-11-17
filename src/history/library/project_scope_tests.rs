use super::*;

fn projects(shared: &Shared, request: Value) -> Result<Value> {
    query(
        shared,
        Operation::List(serde_json::from_value(request).unwrap(), true),
    )
}

fn add(db: &mut Connection, source: &Source, kind: &str, path: Option<&str>, time: &str) -> Entry {
    let id = uuid::Uuid::new_v4().to_string();
    let mut item = candidate(source, &id, &id, path.unwrap_or("/synthetic/unassigned"));
    item.entry.conversation_kind = kind.into();
    item.entry.recorded_at = Some(time.into());
    item.entry.title = "synthetic task".into();
    if kind == "unknown" {
        // Unknown remains primary even when an old boolean says subagent.
        item.entry.is_subagent = true;
    }
    if path.is_none() {
        item.entry.project_id = None;
        item.entry.project_path = None;
    }
    let entry = item.entry.clone();
    store(db, item, vec![]);
    entry
}

#[test]
fn project_scope_filters_before_ordering_pagination_and_unassigned_counts() {
    let (_temp, shared, sources, mut db, _lock) = catalog();
    let a = add(
        &mut db,
        &sources[0],
        "ordinary",
        Some("/synthetic/a/app"),
        "2026-01-01T00:00:00Z",
    );
    let b = add(
        &mut db,
        &sources[0],
        "unknown",
        Some("/synthetic/b/app"),
        "2026-01-02T00:00:00Z",
    );
    add(
        &mut db,
        &sources[0],
        "subagent",
        Some("/synthetic/c"),
        "2026-01-05T00:00:00Z",
    );
    add(
        &mut db,
        &sources[0],
        "background_review",
        Some("/synthetic/d"),
        "2026-01-06T00:00:00Z",
    );
    add(
        &mut db,
        &sources[1],
        "unknown",
        Some("/synthetic/e"),
        "2026-01-07T00:00:00Z",
    );
    add(
        &mut db,
        &sources[0],
        "background_review",
        Some("/synthetic/a/app"),
        "2026-01-08T00:00:00Z",
    );
    for (source, kind) in [
        (&sources[0], "ordinary"),
        (&sources[0], "unknown"),
        (&sources[0], "subagent"),
        (&sources[0], "background_review"),
        (&sources[1], "unknown"),
    ] {
        add(&mut db, source, kind, None, "2026-01-09T00:00:00Z");
    }
    let default = projects(&shared, json!({})).unwrap();
    let all = projects(&shared, json!({"projectScope":"all"})).unwrap();
    assert_eq!(default["records"], all["records"]);
    assert_eq!(all["records"].as_array().unwrap().len(), 5);
    assert_eq!(all["unassignedRecords"], 5);
    let first = projects(&shared, json!({"projectScope":"primary","limit":1})).unwrap();
    assert_eq!(first["records"][0]["projectId"], json!(b.project_id));
    assert_eq!(first["unassignedRecords"], 2);
    let cursor = first["nextCursor"].as_str().unwrap();
    let second = projects(
        &shared,
        json!({"projectScope":"primary","limit":1,"cursor":cursor}),
    )
    .unwrap();
    assert_eq!(second["records"][0]["projectId"], json!(a.project_id));
    assert_eq!(second["records"][0]["entries"], 1);
    assert_eq!(
        second["records"][0]["latestRecordedAt"],
        json!(a.recorded_at)
    );
    assert!(second["nextCursor"].is_null());
    assert_eq!(
        projects(
            &shared,
            json!({"projectScope":"all","limit":1,"cursor":cursor})
        )
        .unwrap_err(),
        "stale_cursor"
    );
    assert_eq!(
        projects(&shared, json!({"projectScope":"primary","projectQ":"/a/"})).unwrap()["records"]
            [0]["projectId"],
        json!(a.project_id)
    );
    assert_eq!(
        projects(
            &shared,
            json!({"projectScope":"primary","sourceId":sources[1].id()})
        )
        .unwrap()["records"],
        json!([])
    );
    assert_eq!(
        projects(
            &shared,
            json!({"projectScope":"primary","sourceId":sources[1].id()})
        )
        .unwrap()["unassignedRecords"],
        0
    );
    assert_eq!(
        projects(
            &shared,
            json!({"projectScope":"primary","q":"synthetic task"})
        )
        .unwrap()["records"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        projects(&shared, json!({"projectScope":"primary","q":"absent"})).unwrap()["records"],
        json!([])
    );
}

#[test]
fn project_scope_is_validated_before_source_access_and_is_projects_only() {
    let (_temp, shared, _sources, _db, _lock) = catalog();
    for scope in ["", "PRIMARY", "auxiliary", "future"] {
        assert_eq!(
            projects(&shared, json!({"projectScope":scope})).unwrap_err(),
            "invalid_query"
        );
    }
    for scope in ["primary", "all"] {
        let q: Query = serde_json::from_value(json!({"projectScope":scope})).unwrap();
        assert_eq!(
            query(&shared, Operation::List(q.clone(), false)).unwrap_err(),
            "invalid_query"
        );
        assert_eq!(
            query(&shared, Operation::Body("missing".into(), q.clone())).unwrap_err(),
            "invalid_query"
        );
        assert_eq!(
            query(&shared, Operation::Details("missing".into(), q.clone())).unwrap_err(),
            "invalid_query"
        );
        assert_eq!(
            query(
                &shared,
                Operation::Body("missing".into(), Query { window: true, ..q })
            )
            .unwrap_err(),
            "invalid_query"
        );
        assert!(serde_json::from_value::<GroupQuery>(json!({"projectScope":scope})).is_err());
    }
}

#[test]
fn project_scope_sql_matches_group_section_for_source_and_conversation_kinds() {
    let db = Connection::open_in_memory().unwrap();
    let source = Source::Native {
        id: "test".into(),
        codex_home: "/synthetic".into(),
    };
    for kind in ["native", "observer", "workbench", "future"] {
        for conversation in [
            "ordinary",
            "unknown",
            "subagent",
            "background_review",
            "future",
            "",
        ] {
            let mut entry = Entry::new(&source, "item");
            entry.kind = kind.into();
            entry.conversation_kind = conversation.into();
            let actual: bool = db
                .query_row(
                    &format!("SELECT {PRIMARY_SECTION_SQL} FROM (SELECT ?1 kind, ?2 metadata)"),
                    params![kind, serde_json::to_string(&entry).unwrap()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                actual,
                section(&entry) == "primary",
                "{kind}/{conversation}"
            );
        }
    }
    for metadata in [json!({}), json!({"conversationKind":null})] {
        let actual: bool = db
            .query_row(
                &format!("SELECT {PRIMARY_SECTION_SQL} FROM (SELECT 'native' kind, ?1 metadata)"),
                [metadata.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(actual, "missing kind evidence remains visible");
    }
}

#[test]
fn project_scope_uses_effective_assignment_and_revocation_without_changing_recorded_cwd() {
    let (_temp, mut shared, sources, mut db, _lock) = catalog();
    let ordinary = add(
        &mut db,
        &sources[0],
        "ordinary",
        Some("/synthetic/original"),
        "2026-01-01T00:00:00Z",
    );
    let review = add(
        &mut db,
        &sources[0],
        "background_review",
        Some("/synthetic/review"),
        "2026-01-02T00:00:00Z",
    );
    let identity = digest(
        &json!([
            ordinary.source_identity,
            ordinary.kind,
            ordinary.native_thread_id
        ])
        .to_string(),
    );
    shared
        .organization
        .save("absent", &ordinary.entry_id, |_| {
            Ok(Some(organization::Override {
                source_identity: ordinary.source_identity.clone(),
                identity_digest: identity,
                disposition: "project".into(),
                project_id: review.project_id.clone(),
                project_path: review.project_path.clone(),
            }))
        })
        .unwrap();
    let page = projects(&shared, json!({"projectScope":"primary"})).unwrap();
    assert_eq!(page["records"].as_array().unwrap().len(), 1);
    assert_eq!(page["records"][0]["projectId"], json!(review.project_id));
    assert_eq!(page["records"][0]["entries"], 1);
    let entries = query(
        &shared,
        Operation::List(
            Query {
                project_id: review.project_id.clone(),
                ..Default::default()
            },
            false,
        ),
    )
    .unwrap();
    assert_eq!(
        entries["selectedProject"]["projectId"],
        json!(review.project_id)
    );
    let moved = entries["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["entryId"] == ordinary.entry_id)
        .unwrap();
    assert_eq!(moved["recordedCwd"], json!(ordinary.recorded_cwd));
    assert_eq!(moved["nativeThreadId"], json!(ordinary.native_thread_id));
    assert_eq!(moved["organization"]["overrideState"], "applied");
    let snapshot = shared.organization.snapshot().unwrap();
    shared.policy = Arc::new(|| {
        Ok(LibraryConfig {
            enabled: false,
            ..Default::default()
        })
    });
    let revoked = projects(&shared, json!({"projectScope":"primary"})).unwrap();
    assert_eq!(revoked["records"], json!([]));
    assert_eq!(revoked["unassignedRecords"], 0);
    assert_eq!(shared.organization.snapshot().unwrap(), snapshot);
}
