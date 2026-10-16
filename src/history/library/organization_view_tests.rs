use super::*;
fn write(entry: &Value, action: &str, project_id: Option<String>) -> OrganizationWrite {
    OrganizationWrite {
        instance_id: uuid::Uuid::new_v4(),
        source_identity: entry["sourceIdentity"].as_str().unwrap().into(),
        source_revision: entry["sourceRevision"].as_str().unwrap().into(),
        action: action.into(),
        project_id,
    }
}

#[test]
fn organization_all_sources_preserve_complete_bytes_and_file_lists_with_external_wal() {
    use crate::workbench::{
        decode::{Change, Decoded, TextKey},
        live::{LiveHub, LiveLimits},
        recording::{Recorder, RecorderOptions, library::Project},
        redaction::RedactionPolicy,
    };
    use std::collections::BTreeMap;

    // Include directory entries as well as complete file bytes: adding an empty
    // directory, deleting a sidecar, or changing audit/journal bytes all fail.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        walkdir::WalkDir::new(root)
            .min_depth(1)
            .into_iter()
            .map(|entry| {
                let entry = entry.unwrap();
                assert!(!entry.file_type().is_symlink());
                let bytes = entry
                    .file_type()
                    .is_file()
                    .then(|| fs::read(entry.path()).unwrap());
                (entry.path().strip_prefix(root).unwrap().to_owned(), bytes)
            })
            .collect()
    }
    let legacy20 = crate::history::tests::Fixture::new(20);
    let legacy25 = crate::history::tests::Fixture::new(25);
    // Fixture includes maintenance_audit/control_audit sentinels in each main
    // database. Keep the real WAL writer in another process throughout checks.
    let _writer20 = crate::history::tests::Writer::start(&legacy20.path, "wal");
    let _writer25 = crate::history::tests::Writer::start(&legacy25.path, "wal");
    for fixture in [&legacy20, &legacy25] {
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", fixture.path.display()));
            assert!(sidecar.is_file());
            assert!(!fs::read(sidecar).unwrap().is_empty());
        }
    }
    let (temp, home, data, config, library) = fixture();
    let cwd = temp.path().join("project");
    fs::create_dir(&cwd).unwrap();
    rollout(
        &home.join("sessions/synthetic.jsonl"),
        uuid::Uuid::new_v4(),
        &cwd,
        "native sentinel",
        "",
    );
    let hub = LiveHub::new(LiveLimits::default());
    let mut options = RecorderOptions::new(data.clone(), &cwd, "journal sentinel".into());
    options.project = Some(Project {
        format_version: 1,
        workspace_id: options.workspace_id.clone(),
        cwd: cwd.to_string_lossy().into(),
        native_home: home.to_string_lossy().into(),
    });
    let recorder = Recorder::start(&hub, options).unwrap();
    let request_id = uuid::Uuid::new_v4();
    hub.apply(Decoded {
        request_id,
        capture_seq: 1,
        received_at: Instant::now(),
        change: Change::TextReplace {
            key: TextKey {
                request_id,
                response_id: Some("synthetic-journal-response".into()),
                wire_item_id: "synthetic-journal-message".into(),
                content_index: 0,
            },
            text: RedactionPolicy::new(vec![])
                .unwrap()
                .scrub("journal body sentinel"),
        },
    });
    let deadline = Instant::now() + Duration::from_secs(4);
    while hub.recorder_status().saved_through_view_seq != hub.snapshot().view_seq {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(recorder);
    let run = data.join("runs").join(hub.epoch().to_string());
    let deadline = Instant::now() + Duration::from_secs(4);
    while !run.join("project.json").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let meta: Value = serde_json::from_slice(&fs::read(run.join("meta.json")).unwrap()).unwrap();
    let journal = run.join(format!(
        "observations.{}.jsonl",
        meta["segments"][0]["id"].as_str().unwrap()
    ));
    assert!(!fs::read(journal).unwrap().is_empty());
    config.write().unwrap().sources = vec![
        Source::Observer {
            id: "old20".into(),
            database: legacy20.path.clone(),
            blob_directory: Some(legacy20.blobs.clone()),
            native_home: None,
        },
        Source::Observer {
            id: "old25".into(),
            database: legacy25.path.clone(),
            blob_directory: Some(legacy25.blobs.clone()),
            native_home: None,
        },
    ];
    let roots: [&Path; 4] = [
        &home,
        legacy20.path.parent().unwrap(),
        legacy25.path.parent().unwrap(),
        &run,
    ];
    let before: Vec<_> = roots.iter().map(|root| snapshot(root)).collect();
    refresh(&library);
    let all = wait(&library, 8);
    let entries = all["records"].as_array().unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry["kind"] == "native")
            .count(),
        1
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry["kind"] == "workbench")
            .count(),
        1
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry["kind"] == "observer")
            .count(),
        6
    );
    let target = project(cwd.canonicalize().unwrap().to_str().unwrap())
        .unwrap()
        .0;
    for entry in entries {
        let id = entry["entryId"].as_str().unwrap().to_owned();
        let read_body = || {
            query(
                &library.handle.shared,
                Operation::Body(
                    id.clone(),
                    Query {
                        window: true,
                        source_revision: entry["sourceRevision"].as_str().map(String::from),
                        ..Default::default()
                    },
                ),
            )
            .unwrap()
        };
        let original_body = read_body();
        let mut revision = query(&library.handle.shared, Operation::Organization(id.clone()))
            .unwrap()["organizationRevision"]
            .as_str()
            .unwrap()
            .to_owned();
        for (action, project_id) in [
            ("project", Some(target.clone())),
            ("unassigned", None),
            ("automatic", None),
        ] {
            let saved = query(
                &library.handle.shared,
                Operation::SaveOrganization(id.clone(), revision, write(entry, action, project_id)),
            )
            .unwrap();
            revision = saved["organizationRevision"].as_str().unwrap().to_owned();
            let current_body = read_body();
            assert_eq!(current_body["records"], original_body["records"]);
            assert_eq!(current_body["entry"]["recordedCwd"], entry["recordedCwd"]);
            assert_eq!(current_body["entry"]["organization"], saved["organization"]);
            for (root, expected) in roots.iter().zip(&before) {
                assert_eq!(
                    &snapshot(root),
                    expected,
                    "source changed during {action}: {}",
                    root.display()
                );
            }
        }
    }
}
#[test]
fn organization_effective_queries_restart_rebuild_and_resume_preserve_original_cwd() {
    let (temp, home, data, config, library) = fixture();
    let first = temp.path().join("a/same");
    let second = temp.path().join("b/same");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    let first = first.canonicalize().unwrap();
    let second = second.canonicalize().unwrap();
    let native_id = uuid::Uuid::new_v4();
    let path = home.join(format!(
        "sessions/rollout-2026-10-08T00-00-00-{native_id}.jsonl"
    ));
    rollout(&path, native_id, &first, "first needle", "");
    rollout(
        &home.join("sessions/target.jsonl"),
        uuid::Uuid::new_v4(),
        &second,
        "target",
        "",
    );
    refresh(&library);
    let entries = wait(&library, 2);
    let original = entries["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["nativeThreadId"] == native_id.to_string())
        .unwrap()
        .clone();
    let id = original["entryId"].as_str().unwrap().to_string();
    let sr = original["sourceRevision"].as_str().unwrap().to_string();
    let target = project(second.to_str().unwrap()).unwrap().0;
    let source_before = fs::read(&path).unwrap();
    let grouped = query(
        &library.handle.shared,
        Operation::Groups(GroupQuery {
            limit: Some(1),
            ..Default::default()
        }),
    )
    .unwrap();
    let body = query(
        &library.handle.shared,
        Operation::Body(
            id.clone(),
            Query {
                window: true,
                ..Default::default()
            },
        ),
    )
    .unwrap();
    let peer_policy = config.clone();
    let peer = HistoryLibrary::with_policy(
        home.clone(),
        data.clone(),
        Arc::new(move || Ok(peer_policy.read().unwrap().clone())),
    )
    .unwrap();
    wait(&peer, 2);
    let saved = query(
        &library.handle.shared,
        Operation::SaveOrganization(
            id.clone(),
            "absent".into(),
            write(&original, "project", Some(target.clone())),
        ),
    )
    .unwrap();
    assert_eq!(
        saved["organization"]["automatic"]["projectPath"],
        first.to_string_lossy().as_ref()
    );
    assert_eq!(saved["organization"]["effective"]["projectId"], target);
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::Groups(GroupQuery {
                limit: Some(1),
                cursor: Some(grouped["nextCursor"].as_str().unwrap().into()),
                ..Default::default()
            })
        )
        .unwrap_err(),
        "stale_cursor"
    );
    let current = query(
        &library.handle.shared,
        Operation::List(
            Query {
                project_id: Some(target.clone()),
                ..Default::default()
            },
            false,
        ),
    )
    .unwrap();
    assert_eq!(current["totalRecords"], 2);
    let peer_current = query(
        &peer.handle.shared,
        Operation::List(
            Query {
                project_id: Some(target.clone()),
                ..Default::default()
            },
            false,
        ),
    )
    .unwrap();
    assert_eq!(peer_current["totalRecords"], 2);
    assert_eq!(
        peer_current["organizationRevision"],
        saved["organizationRevision"]
    );
    drop(peer);
    let changed = query(
        &library.handle.shared,
        Operation::Body(
            id.clone(),
            Query {
                window: true,
                ..Default::default()
            },
        ),
    )
    .unwrap();
    assert_eq!(changed["records"], body["records"]);
    assert_eq!(changed["entry"]["recordedCwd"], original["recordedCwd"]);
    assert_eq!(
        changed["entry"]["projectPath"],
        second.to_string_lossy().as_ref()
    );
    assert_eq!(changed["entry"]["organization"], saved["organization"]);
    let cached = query(
        &library.handle.shared,
        Operation::Body(id.clone(), Query::default()),
    )
    .unwrap();
    assert_eq!(cached["entry"]["organization"], saved["organization"]);
    let sources = library.handle.shared.policy().unwrap().1;
    let selection = launch::resolve(
        &library.handle.shared,
        &sources,
        Some(target.clone()),
        Some((id.clone(), sr.clone())),
    )
    .unwrap();
    assert_eq!(selection.path, first);
    assert_eq!(selection.resume.unwrap().id, native_id);
    assert_eq!(
        launch::resolve(&library.handle.shared, &sources, Some(target.clone()), None)
            .unwrap()
            .path,
        second
    );
    let state_before = fs::read(data.join("organization/state.sqlite")).unwrap();
    drop(library);
    let cache = Connection::open(data.join("library/catalog.sqlite")).unwrap();
    cache.execute_batch("PRAGMA user_version=2;").unwrap();
    drop(cache);
    let policy = config.clone();
    let library = HistoryLibrary::with_policy(
        home,
        data.clone(),
        Arc::new(move || Ok(policy.read().unwrap().clone())),
    )
    .unwrap();
    let rebuilt = wait(&library, 2);
    assert_eq!(rebuilt["totalRecords"], 2);
    assert_eq!(
        fs::read(data.join("organization/state.sqlite")).unwrap(),
        state_before
    );
    assert_eq!(fs::read(&path).unwrap(), source_before);
    let current = query(&library.handle.shared, Operation::Organization(id.clone())).unwrap();
    assert_eq!(current["organization"]["effective"]["projectId"], target);
    let unassigned = query(
        &library.handle.shared,
        Operation::SaveOrganization(
            id.clone(),
            current["organizationRevision"].as_str().unwrap().into(),
            write(&original, "unassigned", None),
        ),
    )
    .unwrap();
    assert_eq!(
        unassigned["organization"]["unassignedReason"],
        "user_unassigned"
    );
    let reset = query(
        &library.handle.shared,
        Operation::SaveOrganization(
            id.clone(),
            unassigned["organizationRevision"].as_str().unwrap().into(),
            write(&original, "automatic", None),
        ),
    )
    .unwrap();
    assert_eq!(
        reset["organization"]["effective"]["projectPath"],
        first.to_string_lossy().as_ref()
    );
    assert_eq!(reset["organization"]["overrideState"], "none");
}
#[test]
fn organization_invalid_first_targets_and_stale_source_do_not_create_state() {
    let (temp, home, data, _config, library) = fixture();
    let cwd = temp.path().join("project");
    fs::create_dir(&cwd).unwrap();
    rollout(
        &home.join("sessions/a.jsonl"),
        uuid::Uuid::new_v4(),
        &cwd,
        "source",
        "",
    );
    refresh(&library);
    let entries = wait(&library, 1);
    let original = &entries["records"][0];
    let id = original["entryId"].as_str().unwrap().into();
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::SaveOrganization(
                id,
                "absent".into(),
                write(original, "project", Some("p_fake".into()))
            )
        )
        .unwrap_err(),
        "project_unavailable"
    );
    assert!(!data.join("organization").exists());
    let mut request = write(original, "unassigned", None);
    request.source_revision = "old".into();
    assert_eq!(
        query(
            &library.handle.shared,
            Operation::SaveOrganization(
                original["entryId"].as_str().unwrap().into(),
                "absent".into(),
                request
            )
        )
        .unwrap_err(),
        "source_revision_changed"
    );
    assert!(!data.join("organization").exists());
}

#[test]
fn organization_failure_is_explicit_but_cannot_block_source_reading_or_original_resume() {
    let (temp, home, data, _config, library) = fixture();
    let first = temp.path().join("a");
    fs::create_dir(&first).unwrap();
    let first = first.canonicalize().unwrap();
    let thread = uuid::Uuid::new_v4();
    let path = home.join(format!(
        "sessions/rollout-2026-10-08T00-00-00-{thread}.jsonl"
    ));
    rollout(&path, thread, &first, "source durable", "");
    refresh(&library);
    let original = wait(&library, 1)["records"][0].clone();
    let id = original["entryId"].as_str().unwrap().to_string();
    query(
        &library.handle.shared,
        Operation::SaveOrganization(
            id.clone(),
            "absent".into(),
            write(&original, "unassigned", None),
        ),
    )
    .unwrap();
    fs::write(
        data.join("organization/state.sqlite"),
        "corrupted-owned-state",
    )
    .unwrap();
    for window in [false, true] {
        let source = query(
            &library.handle.shared,
            Operation::Body(
                id.clone(),
                Query {
                    window,
                    ..Default::default()
                },
            ),
        )
        .unwrap();
        assert!(!source["records"].as_array().unwrap().is_empty());
        assert_eq!(
            source["entry"]["organization"]["overrideState"],
            "unavailable"
        );
    }
    assert!(
        query(
            &library.handle.shared,
            Operation::List(Query::default(), false)
        )
        .is_err()
    );
    assert!(query(&library.handle.shared, Operation::Organization(id.clone())).is_err());
    let sources = library.handle.shared.policy().unwrap().1;
    assert_eq!(
        launch::resolve(
            &library.handle.shared,
            &sources,
            Some("p_effective-unavailable".into()),
            Some((id, original["sourceRevision"].as_str().unwrap().into()))
        )
        .unwrap()
        .path,
        first
    );
    assert!(
        launch::resolve(
            &library.handle.shared,
            &sources,
            original["projectId"].as_str().map(String::from),
            None
        )
        .is_err()
    );
}

#[test]
fn organization_revoked_overrides_do_not_fabricate_projects_or_leak_saved_paths() {
    let (temp, home, _data, config, library) = fixture();
    let cwd = temp.path().join("project");
    fs::create_dir(&cwd).unwrap();
    rollout(
        &home.join("sessions/a.jsonl"),
        uuid::Uuid::new_v4(),
        &cwd,
        "source",
        "",
    );
    refresh(&library);
    let original = wait(&library, 1)["records"][0].clone();
    let id = original["entryId"].as_str().unwrap().to_string();
    query(
        &library.handle.shared,
        Operation::SaveOrganization(
            id.clone(),
            "absent".into(),
            write(
                &original,
                "project",
                original["projectId"].as_str().map(String::from),
            ),
        ),
    )
    .unwrap();
    config.write().unwrap().enabled = false;
    for projects in [true, false] {
        let page = query(
            &library.handle.shared,
            Operation::List(Query::default(), projects),
        )
        .unwrap();
        assert_eq!(page["records"], json!([]));
    }
    assert_eq!(
        query(&library.handle.shared, Operation::Organization(id.clone())).unwrap_err(),
        "entry_unavailable"
    );
    config.write().unwrap().enabled = true;
    refresh(&library);
    wait(&library, 1);
    assert_eq!(
        query(&library.handle.shared, Operation::Organization(id)).unwrap()["organization"]["overrideState"],
        "applied"
    );
}
