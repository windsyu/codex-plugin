//! R5 acceptance against a temporary schema-20 database copy, never user data.
#![cfg(test)]

use super::*;
use crate::ingest::Importer;
use anyhow::Context;
use std::fs;
use std::path::Path as FsPath;
use tempfile::TempDir;
use tower::ServiceExt;

fn token() -> String {
    URL_SAFE_NO_PAD.encode([9_u8; 32])
}

fn copy_tree(from: &FsPath, to: &FsPath) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn tree_bytes(root: &FsPath) -> Result<std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>> {
    fn visit(
        root: &FsPath,
        path: &FsPath,
        result: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    ) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                visit(root, &entry.path(), result)?;
            } else {
                result.insert(
                    entry.path().strip_prefix(root)?.to_owned(),
                    fs::read(entry.path())?,
                );
            }
        }
        Ok(())
    }
    let mut result = std::collections::BTreeMap::new();
    visit(root, root, &mut result)?;
    Ok(result)
}

async fn query(app: &Router, uri: &str) -> Result<Value> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {}", token()))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024).await?;
    assert!(!String::from_utf8_lossy(&bytes).contains("fixture-secret"));
    Ok(serde_json::from_slice(&bytes)?)
}

#[tokio::test]
async fn schema20_copy_v1_queries_export_and_rejected_mutations_preserve_history_and_audit()
-> Result<()> {
    assert_eq!(
        LATEST_SCHEMA_VERSION, 20,
        "update the compatibility fixture explicitly if the legacy schema changes"
    );
    let temp = TempDir::new()?;
    let native = temp.path().join("native");
    copy_tree(
        &FsPath::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex-home"),
        &native,
    )?;
    let rollout = native.join("sessions/2026/08/14/rollout-2026-08-14T12-00-00-00000000-0000-7000-8000-000000000001.jsonl");
    let fixture = fs::read_to_string(&rollout)?.replace(
        "实现一个只读的本地会话观察器",
        "R5_HISTORY_SEARCH synthetic legacy message",
    );
    fs::write(&rollout, fixture)?;
    let native_before = tree_bytes(&native)?;
    let mut config = Config::default();
    config.sources[0].codex_home = native.clone();
    config.storage.database = temp.path().join("original/observer.sqlite");
    config.storage.blob_dir = temp.path().join("original/blobs");
    config.storage.fingerprint_key_file = temp.path().join("fingerprint.key");
    let original =
        Database::open_with_blobs(&config.storage.database, &config.storage.blob_dir, 64)?;
    original.migrate()?;
    Importer::new(&config, &original)?.import_all()?;
    let connection = original.connect()?;
    connection.execute_batch("INSERT INTO gateway_commands(command_id,principal_id,capability,idempotency_key,payload_hash,source_id,source_epoch,input_summary_json,state,created_at_ms,updated_at_ms)
        VALUES ('r5-command','local_bearer','turn.start','r5-key','hash','r5-source','r5-epoch','{}','received',1,1);
        INSERT INTO command_transitions(command_id,from_state,to_state,occurred_at_ms,details_summary_json) VALUES ('r5-command',NULL,'received',1,'{}');
        INSERT INTO control_audit(command_id,principal_id,capability,source_id,source_epoch,decision,outcome,payload_hash,input_summary_json,occurred_at_ms)
        VALUES ('r5-command','local_bearer','turn.start','r5-source','r5-epoch','received','pending','hash','{}',1);
        INSERT INTO maintenance_audit VALUES ('r5-audit','retention','observer','fixture',1,'{}');
        PRAGMA wal_checkpoint(TRUNCATE);
        PRAGMA journal_mode=DELETE;")?;
    drop(connection);
    drop(original);
    let original_before = tree_bytes(&temp.path().join("original"))?;
    let copy = temp.path().join("copy");
    copy_tree(&temp.path().join("original"), &copy)?;
    let before = tree_bytes(&copy)?;
    let database = Arc::new(Database::open_read_only_with_blobs(
        &copy.join("observer.sqlite"),
        &copy.join("blobs"),
        64,
    )?);
    let read = database.connect_read_only()?;
    assert!(read.execute("DELETE FROM maintenance_audit", []).is_err());
    let thread_key: String = read.query_row("SELECT thread_key FROM threads WHERE codex_thread_id='00000000-0000-7000-8000-000000000001'", [], |r| r.get(0))?;
    let event_seq = database.max_event_seq()?;
    let audit_before: String = read.query_row("SELECT json_group_array(json_object('id',command_id,'decision',decision,'outcome',outcome)) FROM control_audit", [], |r| r.get(0))?;
    drop(read);
    let state = ApiState {
        writer: WriterHandle::start(database.clone(), 4096, 16, 512)?,
        database: database.clone(),
        token: Arc::new(token()),
        strict_origin: true,
        allowed_origins: Arc::new(vec!["http://127.0.0.1:4765".into()]),
        blob_downloads: Arc::new(Semaphore::new(1)),
        settings: Arc::new(json!({})),
        tailscale: None,
    };
    let app = application(state);
    let unauthorized = app
        .clone()
        .oneshot(Request::builder().uri("/v1/threads").body(Body::empty())?)
        .await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let first = query(&app, "/v1/threads?limit=1").await?;
    assert_eq!(first["apiVersion"], "v1");
    assert_eq!(first["asOfEventSeq"], event_seq);
    assert!(first.get("viewSeq").is_none());
    let cursor = first["nextCursor"]
        .as_str()
        .context("fixture must span multiple pages")?;
    let second = query(&app, &format!("/v1/threads?limit=1&cursor={cursor}")).await?;
    assert_ne!(
        first["data"][0]["threadKey"],
        second["data"][0]["threadKey"]
    );
    assert_eq!(first["asOfEventSeq"], second["asOfEventSeq"]);
    // R6-A reads the same synthetic schema-20 history independently. No old
    // Database/Importer/Writer dependency is used by the new adapter itself.
    // Compare source facts; the library has separate pagination/provenance DTOs.
    #[cfg(unix)]
    {
        use codex_local_observer::history::legacy_reader::{
            Collection, LegacyReader, Query as LegacyQuery, ReadBudget,
        };
        let reader = LegacyReader::open(
            "synthetic-v1",
            &copy.join("observer.sqlite"),
            Some(&copy.join("blobs")),
            &ReadBudget::default(),
        )?;
        for (collection, route, key, fields) in [
            (
                Collection::Threads,
                "/v1/threads?limit=100".into(),
                "threadKey",
                vec![
                    "threadKey",
                    "codexThreadId",
                    "storeSourceId",
                    "name",
                    "model",
                    "archived",
                    "status",
                    "stale",
                    "captureCompleteness",
                    "completenessReasons",
                    "recencyAtMs",
                    "lastMessagePreview",
                    "lastEventSeq",
                ],
            ),
            (
                Collection::Turns,
                format!("/v1/threads/{thread_key}/turns?limit=100"),
                "turnId",
                vec![
                    "turnId",
                    "status",
                    "captureCompleteness",
                    "completenessReasons",
                    "coverage",
                    "startedAtMs",
                    "completedAtMs",
                    "raw",
                    "lastEventSeq",
                ],
            ),
            (
                Collection::Items,
                format!("/v1/threads/{thread_key}/items?limit=100"),
                "itemId",
                vec![
                    "turnScope",
                    "itemId",
                    "turnId",
                    "itemType",
                    "status",
                    "summaryText",
                    "raw",
                    "provenance",
                    "lastEventSeq",
                ],
            ),
        ] {
            let previous = query(&app, &route).await?;
            let current = reader.page(
                &LegacyQuery {
                    collection,
                    scope: (collection != Collection::Threads).then(|| thread_key.clone()),
                },
                None,
                100,
                &ReadBudget::default(),
            )?;
            let previous = previous["data"].as_array().context("V1 page")?;
            assert_eq!(current.records.len(), previous.len());
            for record in current.records {
                let expected = previous
                    .iter()
                    .find(|row| row[key] == record.fields[key])
                    .context("matching V1 identity")?;
                for field in &fields {
                    assert_eq!(
                        record.fields[*field], expected[*field],
                        "{collection:?} {field}"
                    );
                }
            }
        }
    }
    for route in [
        format!("/v1/threads/{thread_key}"),
        format!("/v1/threads/{thread_key}/turns"),
        format!("/v1/threads/{thread_key}/items"),
        format!("/v1/threads/{thread_key}/events"),
        "/v1/projects".into(),
        "/v1/events".into(),
        "/v1/search?q=R5_HISTORY_SEARCH".into(),
    ] {
        let result = query(&app, &route).await?;
        assert_eq!(result["asOfEventSeq"], event_seq);
        if let Some(items) = result["data"].as_array() {
            assert!(!items.is_empty(), "{route}");
        }
    }
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        for uri in [
            "/v1/threads".to_owned(),
            format!("/v1/threads/{thread_key}"),
            "/v1/events".into(),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header(header::AUTHORIZATION, format!("Bearer {}", token()))
                        .header(header::ORIGIN, "http://127.0.0.1:4765")
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        }
    }
    for uri in [
        "/v1/commands",
        "/v1/sessions",
        "/v1/requests/r5/actions",
        "/v2/sessions",
        "/v2/sessions/fake",
        "/v2/sessions/old/attach",
        "/v2/sessions/old/input-lease",
        "/v2/sessions/old/interrupt",
        "/v2/sessions/old/stop",
        "/v2/commands",
        "/v2/requests/old/actions",
        "/v2/uploads/images",
        "/v2/threads",
        "/v2/threads/old/inputs",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {}", token()))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    for uri in [
        "/v2/stream",
        "/v2/session-sources",
        "/v2/sessions/old",
        "/v2/sessions/old/terminal",
        "/v2/sessions/old/events",
        "/v2/commands",
        "/v2/commands/old",
        "/v2/control/sources",
        "/v2/control/catalog",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {}", token()))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
    }
    let health = query(&app, "/v1/health").await?;
    assert_eq!(health["data"]["control"]["enabled"], false);
    assert_eq!(health["data"]["control"]["retired"], true);
    let capabilities = query(&app, "/v1/meta/capabilities").await?;
    assert_eq!(capabilities["data"]["mutationRoutes"], json!([]));
    assert_eq!(capabilities["data"]["live"]["enabled"], false);
    let export = temp.path().join("export.json");
    let report = database.export_thread(&thread_key, &export)?;
    assert!(report.raw_events > 0 && report.turns > 0);
    let exported = fs::read_to_string(&export)?;
    assert!(exported.contains("codex-local-observer-export-v1"));
    assert!(!exported.contains("fixture-secret"));
    assert!(
        database.export_thread(&thread_key, &export).is_err(),
        "export must not overwrite an existing file"
    );
    let read = database.connect_read_only()?;
    let audit_after: String = read.query_row("SELECT json_group_array(json_object('id',command_id,'decision',decision,'outcome',outcome)) FROM control_audit", [], |r| r.get(0))?;
    assert_eq!(audit_before, audit_after);
    assert_eq!(
        tree_bytes(&copy)?,
        before,
        "queries/export must preserve every legacy database/blob byte"
    );
    assert_eq!(tree_bytes(&temp.path().join("original"))?, original_before);
    assert_eq!(tree_bytes(&native)?, native_before);
    println!(
        "R5 schema20: authenticated V1 queries/export, retired V2 routes rejected, byte-identical database/blobs/audit/native history"
    );
    Ok(())
}
