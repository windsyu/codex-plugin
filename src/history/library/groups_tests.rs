use super::*;
#[path = "groups_perf_tests.rs"]
mod performance;

fn catalog() -> (
    tempfile::TempDir,
    Shared,
    Vec<Source>,
    Connection,
    std::fs::File,
) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("native");
    std::fs::create_dir(&home).unwrap();
    let data = temp.path().join("history");
    let config = LibraryConfig {
        sources: vec![Source::Observer {
            id: "observer".into(),
            database: temp.path().join("old.db"),
            blob_directory: None,
            native_home: Some(home.clone()),
        }],
        ..Default::default()
    };
    let shared = Shared {
        home,
        organization: organization::Store::new(data.clone()),
        data,
        policy: Arc::new(move || Ok(config.clone())),
        status: RwLock::new(vec![]),
        stop: AtomicBool::new(false),
        refresh: AtomicBool::new(false),
        refresh_status: RwLock::new(RefreshStatus::default()),
        key: [3; 32],
    };
    let sources = shared.policy().unwrap().1;
    let (_, lock, db) = open_cache(&shared).unwrap();
    for source in &sources {
        db.execute(
            "INSERT INTO catalog_sources VALUES (?1,?2,?1,'r1','[]')",
            params![source.id(), source.identity()],
        )
        .unwrap();
        shared.set(status(source, "ready", "", 0, Some("r1".into())));
    }
    (temp, shared, sources, db, lock)
}
fn store(db: &mut Connection, c: Candidate, records: Vec<Value>) {
    let g = c.entry.source_id.clone();
    write_document(
        db,
        &g,
        &Document {
            entry: c.entry,
            locator: c.locator,
            records,
        },
        None,
        4 * 1024 * 1024,
    )
    .unwrap();
}

#[test]
fn full_group_uses_same_normalized_uuid_as_list_for_both_anchors() {
    let (_temp, shared, sources, mut db, _lock) = catalog();
    let native = candidate(&sources[0], THREAD, THREAD, "/tmp/project");
    let observer = candidate(&sources[2], "one", &THREAD.to_uppercase(), "/tmp/project");
    let anchors = [
        native.entry.entry_id.clone(),
        observer.entry.entry_id.clone(),
    ];
    store(&mut db, native, vec![]);
    store(&mut db, observer, vec![]);
    let page = list(&shared, &sources, GroupQuery::default()).unwrap();
    assert_eq!(page["counts"]["primary"]["groups"], 1);
    for anchor_entry_id in anchors {
        let result = full(
            &shared,
            &sources,
            page["records"][0]["groupId"].as_str().unwrap().into(),
            GroupAnchorQuery {
                anchor_entry_id,
                revision: page["revision"].as_str().unwrap().into(),
            },
        )
        .unwrap();
        assert_eq!(result["members"].as_array().unwrap().len(), 2);
        assert!(result.get("body").is_none());
    }
}

#[test]
fn search_counts_sections_cursors_and_global_qualification_are_source_scoped() {
    let (_temp, shared, sources, mut db, _lock) = catalog();
    let native = candidate(&sources[0], THREAD, THREAD, "/tmp/project");
    let native_id = native.entry.entry_id.clone();
    let observer = candidate(&sources[2], "one", THREAD, "/tmp/project");
    let observer_id = observer.entry.entry_id.clone();
    store(&mut db, native, vec![json!({"text":"native unrelated"})]);
    store(&mut db, observer, vec![json!({"text":"observer needle"})]);
    let page = list(
        &shared,
        &sources,
        GroupQuery {
            q: Some("needle".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(page["counts"]["primary"], json!({"groups":1,"records":1}));
    assert_eq!(page["records"][0]["preferredEntryId"], observer_id);
    assert_eq!(page["records"][0]["members"][0]["match"]["record"], 0);
    let all = full(
        &shared,
        &sources,
        page["records"][0]["groupId"].as_str().unwrap().into(),
        GroupAnchorQuery {
            anchor_entry_id: observer_id,
            revision: page["revision"].as_str().unwrap().into(),
        },
    )
    .unwrap();
    assert_eq!(all["members"].as_array().unwrap().len(), 2);
    assert!(all["members"][0].get("match").is_none());
    store(
        &mut db,
        candidate(&sources[2], "two", THREAD, "/tmp/other"),
        vec![],
    );
    let filtered = list(
        &shared,
        &sources,
        GroupQuery {
            source_id: Some("default-native".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_ne!(
        filtered["records"][0]["groupId"],
        page["records"][0]["groupId"]
    );
    assert_eq!(filtered["records"][0]["members"][0]["entryId"], native_id);
    let thread = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let mut agent = candidate(&sources[0], thread, thread, "/tmp/project");
    agent.entry.conversation_kind = "subagent".into();
    store(&mut db, agent, vec![json!({"text":"needle"})]);
    let mut run = candidate(&sources[1], "run", THREAD, "/tmp/project");
    run.locator = json!({"run":"run"});
    store(&mut db, run, vec![json!({"text":"needle"})]);
    let first = list(
        &shared,
        &sources,
        GroupQuery {
            limit: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        first["counts"],
        json!({"primary":{"groups":3,"records":3},"auxiliary":{"groups":1,"records":1},"runs":{"groups":1,"records":1}})
    );
    let cursor = first["nextCursor"].as_str().unwrap().to_string();
    assert!(
        list(
            &shared,
            &sources,
            GroupQuery {
                limit: Some(1),
                cursor: Some(cursor.clone()),
                ..Default::default()
            }
        )
        .is_ok()
    );
    assert_eq!(
        list(
            &shared,
            &sources,
            GroupQuery {
                q: Some("x".into()),
                limit: Some(1),
                cursor: Some(cursor.clone()),
                ..Default::default()
            }
        )
        .unwrap_err(),
        "stale_cursor"
    );
    db.execute(
        "UPDATE catalog_sources SET revision='r2' WHERE id='observer'",
        [],
    )
    .unwrap();
    assert_eq!(
        list(
            &shared,
            &sources,
            GroupQuery {
                limit: Some(1),
                cursor: Some(cursor),
                ..Default::default()
            }
        )
        .unwrap_err(),
        "stale_cursor"
    );
}

fn candidate(source: &Source, key: &str, thread: &str, project_path: &str) -> Candidate {
    let mut entry = Entry::new(source, key);
    entry.native_thread_id = Some(thread.into());
    entry.path(project_path);
    entry.source_revision = "r1".into();
    entry.conversation_kind = "unknown".into();
    Candidate {
        entry,
        locator: json!({"thread":key}),
        matches: true,
    }
}
fn sources() -> Vec<Source> {
    vec![
        Source::Native {
            id: "native".into(),
            codex_home: "/tmp/r7-home".into(),
        },
        Source::Observer {
            id: "observer".into(),
            database: "/tmp/r7.db".into(),
            blob_directory: None,
            native_home: Some("/tmp/r7-home".into()),
        },
    ]
}
const THREAD: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

#[test]
fn a_single_member_override_splits_a_group_without_changing_another_source() {
    let (_temp, shared, sources, mut db, _lock) = catalog();
    let native = candidate(&sources[0], THREAD, THREAD, "/tmp/project");
    let native_entry = native.entry.clone();
    store(&mut db, native, vec![]);
    store(
        &mut db,
        candidate(&sources[2], "one", THREAD, "/tmp/project"),
        vec![],
    );
    let target = candidate(
        &sources[0],
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        "/tmp/target",
    );
    let target_id = target.entry.project_id.clone();
    store(&mut db, target, vec![]);
    let before = list(&shared, &sources, GroupQuery::default()).unwrap();
    assert_eq!(before["counts"]["primary"]["groups"], 2);
    organization_view::save(
        &shared,
        &sources,
        native_entry.entry_id.clone(),
        "absent".into(),
        OrganizationWrite {
            instance_id: uuid::Uuid::new_v4(),
            source_identity: native_entry.source_identity,
            source_revision: native_entry.source_revision,
            action: "project".into(),
            project_id: target_id,
        },
    )
    .unwrap();
    let after = list(&shared, &sources, GroupQuery::default()).unwrap();
    assert_eq!(after["counts"]["primary"], json!({"groups":3,"records":3}));
    let observer = after["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["members"][0]["sourceId"] == "observer")
        .unwrap();
    assert_eq!(observer["projectPath"], "/tmp/project");
    let old_group = before["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["members"].as_array().unwrap().len() == 2)
        .unwrap();
    assert_eq!(
        full(
            &shared,
            &sources,
            old_group["groupId"].as_str().unwrap().into(),
            GroupAnchorQuery {
                anchor_entry_id: native_entry.entry_id,
                revision: before["revision"].as_str().unwrap().into()
            }
        )
        .unwrap_err(),
        "group_changed"
    );
}

#[test]
fn qualification_precedes_filters_and_never_crops_ambiguous_members() {
    let sources = sources();
    let mut entries = vec![
        candidate(&sources[0], THREAD, THREAD, "/tmp/a/same"),
        candidate(&sources[1], "one", THREAD, "/tmp/a/same"),
    ];
    let ids = group_ids(&sources, &entries);
    assert_eq!(ids[0], ids[1]);
    entries.push(candidate(&sources[1], "two", THREAD, "/tmp/b/same"));
    let ids = group_ids(&sources, &entries);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
    entries[2].matches = false;
    assert_eq!(group_ids(&sources, &entries), ids);
}

#[test]
fn home_kind_identity_and_project_conflicts_split_groups() {
    let mut sources = sources();
    let base = candidate(&sources[0], THREAD, THREAD, "/tmp/a/same");
    let other = candidate(&sources[1], "one", THREAD, "/tmp/a/same");
    for field in ["kind", "identity", "project"] {
        let mut changed = other.clone();
        match field {
            "kind" => changed.entry.conversation_kind = "subagent".into(),
            "identity" => changed.entry.source_identity = "wrong".into(),
            _ => changed.entry.path("/tmp/b/same"),
        }
        let ids = group_ids(&sources, &[base.clone(), changed]);
        assert_ne!(ids[0], ids[1], "{field}");
    }
    if let Source::Observer { native_home, .. } = &mut sources[1] {
        *native_home = Some("/tmp/other-home".into());
    }
    assert_ne!(
        group_ids(&sources, &[base, other])[0],
        group_ids(
            &sources,
            &[
                candidate(&sources[0], THREAD, THREAD, "/tmp/a/same"),
                candidate(&sources[1], "one", THREAD, "/tmp/a/same")
            ]
        )[1]
    );
}

#[test]
fn unknown_conflicting_kind_evidence_cannot_fold_with_healthy_unknown() {
    let sources = sources();
    let native = candidate(&sources[0], THREAD, THREAD, "/tmp/project");
    let observer = candidate(&sources[1], "one", THREAD, "/tmp/project");
    for basis in ["metadata_conflict_v1", "metadata_invalid_v1"] {
        let mut changed = native.clone();
        changed.entry.kind_basis = basis.into();
        let ids = group_ids(&sources, &[changed, observer.clone()]);
        assert_ne!(ids[0], ids[1], "{basis}");
    }
}
