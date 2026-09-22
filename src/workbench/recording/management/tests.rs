use super::super::{
    fs::Directory,
    journal::{Meta, Writer},
};
use super::*;
use crate::workbench::{
    config::{ConfigService, Overrides, Prepared},
    live::{LiveHub, LiveLimits},
};
use std::os::unix::fs::PermissionsExt;
mod process;

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    workspace: String,
    current: Uuid,
    config: ConfigService,
}
impl Fixture {
    fn enable_retention(&self, days: u32) {
        let mut config = crate::workbench::config::Config::default();
        config.history.cleanup.enabled = true;
        config.history.cleanup.retention.enabled = true;
        config.history.cleanup.retention.days = days;
        std::fs::write(
            self._tmp.path().join(".codex-web/config/config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
    }
    fn proof(&self, id: Uuid, ended: chrono::DateTime<chrono::Utc>) {
        let dir = Directory::root(&self.root)
            .unwrap()
            .dir("runs", false)
            .unwrap()
            .dir(&id.to_string(), false)
            .unwrap();
        let meta = super::super::journal::read_meta(&dir, id).unwrap();
        let proof = scan::Lifecycle {
            schema_version: 1,
            run_epoch: id,
            workspace_id: meta.workspace_id,
            ended_at: ended.to_rfc3339(),
            final_meta_digest: blake3::hash(&dir.read("meta.json", 4 * 1024 * 1024).unwrap())
                .to_hex()
                .to_string(),
            saved_record_seq: meta.saved_record_seq,
        };
        dir.atomic("lifecycle.json", &serde_json::to_vec(&proof).unwrap())
            .unwrap();
    }
    fn enable_manual(&self, enabled: bool) {
        let mut config = crate::workbench::config::Config::default();
        config.history.cleanup.enabled = enabled;
        std::fs::write(
            self._tmp.path().join(".codex-web/config/config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
    }
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cwd = tmp.path().join("project");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&cwd).unwrap();
        let p = Prepared::load(
            &home,
            &cwd,
            &crate::workbench::paths::WorkbenchPaths::from_user_home(home.parent().unwrap())
                .unwrap(),
            None,
            Overrides::default(),
        )
        .unwrap();
        let root = p.data_dir.clone();
        let config = ConfigService::start(p).unwrap();
        Directory::root(&root).unwrap().dir("runs", true).unwrap();
        Self {
            _tmp: tmp,
            root,
            workspace: blake3::hash(cwd.as_os_str().as_encoded_bytes())
                .to_hex()
                .to_string(),
            current: Uuid::new_v4(),
            config,
        }
    }
    fn run(&self, other: bool, ended: bool) -> (Uuid, Writer) {
        let hub = LiveHub::new(LiveLimits::default());
        let id = hub.epoch();
        let m = Meta {
            format_version: 1,
            run_epoch: id,
            workspace_id: if other {
                "b".repeat(64)
            } else {
                self.workspace.clone()
            },
            project_name: "Synthetic history".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            ended: false,
            persisted_through_view_seq: 0,
            saved_through_view_seq: 0,
            saved_record_seq: 0,
            segments: vec![],
            gaps: vec![],
        };
        let mut w = Writer::create(
            &Directory::root(&self.root).unwrap(),
            m,
            hub.recording_checkpoint(),
        )
        .unwrap();
        w.commit(ended).unwrap();
        // These fixtures represent pre-R3.1 manifests without clean-end proof.
        let _ = w.dir.remove("lifecycle.json");
        (id, w)
    }
    fn engine(&self) -> Engine {
        Engine::new(
            self.root.clone(),
            self.workspace.clone(),
            self.current,
            self.config.handle(),
        )
    }
}
#[test]
fn clean_end_proof_binds_final_manifest_without_changing_v1_reader_or_watermarks() {
    let f = Fixture::new();
    let (id, mut writer) = f.run(false, false);
    writer.commit(true).unwrap();
    let meta = super::super::journal::read_meta(&writer.dir, id).unwrap();
    let bytes = writer.dir.read("meta.json", 4096).unwrap();
    let digest = blake3::hash(&bytes).to_hex().to_string();
    assert!(scan::ended_at(&writer.dir, &meta, &digest, chrono::Utc::now()).is_ok());
    assert!(super::super::journal::recover(&writer.dir, &meta, None).is_ok());
    assert!(
        !serde_json::from_slice::<Value>(&bytes)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("endedAt")
    );
    writer.dir.remove("lifecycle.json").unwrap();
    let mut checks = 0;
    writer
        .commit_when(|| {
            checks += 1;
            checks == 1
        })
        .unwrap();
    assert!(writer.dir.open("lifecycle.json", false).is_err());
    writer.dir.dir("lifecycle.json", true).unwrap();
    writer.commit(true).unwrap();
    assert_eq!(
        super::super::journal::read_meta(&writer.dir, id)
            .unwrap()
            .saved_record_seq,
        meta.saved_record_seq
    );
    assert!(scan::ended_at(&writer.dir, &writer.meta, &digest, chrono::Utc::now()).is_err());
}
#[test]
fn automatic_retention_requires_opt_in_and_only_removes_proven_expired_inactive_current_project_runs()
 {
    let f = Fixture::new();
    let (expired, w) = f.run(false, true);
    drop(w);
    let (legacy, w) = f.run(false, true);
    drop(w);
    let (unclean, w) = f.run(false, false);
    drop(w);
    let (future, w) = f.run(false, true);
    drop(w);
    let (other, w) = f.run(true, true);
    drop(w);
    let (active, _writer) = f.run(false, true);
    let now = chrono::Utc::now();
    for id in [expired, unclean, other, active] {
        f.proof(id, now - chrono::Duration::days(2));
    }
    f.proof(future, now + chrono::Duration::days(2));
    let mut e = f.engine();
    finish_scan(&mut e);
    assert!(e.retention.last_job.is_none());
    assert!(!f.root.join("cleanup").exists());
    f.enable_retention(1);
    e.retention.policy_at = Instant::now();
    for _ in 0..10000 {
        e.tick();
        if e.retention.last_job.is_some() && e.cleanup.is_none() {
            break;
        }
    }
    let id = e
        .retention
        .last_job
        .expect("automatic cleanup was scheduled");
    let job = e.query(Query::Job { id }).unwrap();
    assert_eq!(job["result"]["mode"], "retention");
    assert_eq!(job["result"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(job["result"]["items"][0]["state"], "deleted");
    for id in [legacy, unclean, future, other, active] {
        assert!(f.root.join("runs").join(id.to_string()).exists());
    }
    assert!(!f.root.join("runs").join(expired.to_string()).exists());
    assert!(e.retention.skipped.contains_key("ended_at_unknown"));
    assert!(e.retention.skipped.contains_key("clock_invalid"));
    assert!(e.retention.due.saturating_duration_since(Instant::now()) > Duration::from_secs(3500));
}
#[test]
fn retention_boundary_future_proof_policy_changes_and_clock_jumps_are_conservative() {
    let f = Fixture::new();
    let (id, w) = f.run(false, true);
    drop(w);
    let now = chrono::Utc::now();
    f.proof(id, now - chrono::Duration::days(1));
    f.enable_retention(1);
    let mut e = f.engine();
    let mut p = preview(&mut e, PreviewMode::Retention(1));
    let row = p.items[0].run.as_ref().unwrap();
    let ended = chrono::DateTime::parse_from_rfc3339(row.ended_at.as_ref().unwrap())
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(!scan::expired(
        row,
        1,
        ended + chrono::Duration::seconds(86399)
    ));
    assert!(scan::expired(
        row,
        1,
        ended + chrono::Duration::seconds(86400)
    ));
    p.mode = "retention".into();
    p.executable = true;
    let mut task = cleanup::Task::create_automatic(
        &f.root,
        &f.workspace,
        f.current,
        f.config.handle(),
        &p,
        p.config_revision.as_deref().unwrap(),
        1,
    )
    .unwrap();
    f.enable_retention(2);
    complete_task(&mut task);
    assert_eq!(task.job.value()["items"][0]["state"], "skipped");
    assert_eq!(task.job.value()["items"][0]["reason"], "not_expired");
    assert!(f.root.join("runs").join(id.to_string()).exists());
    drop(task);
    let mut clock = retention::Scheduler::new();
    clock.observe_clock(
        Instant::now(),
        chrono::Utc::now() + chrono::Duration::hours(2),
    );
    assert!(clock.clock_blocked);
    assert_eq!(clock.state, "clock_changed");
}

#[test]
fn automatic_creation_failure_is_visible_and_backs_off_before_retrying() {
    let f = Fixture::new();
    let (id, w) = f.run(false, true);
    drop(w);
    f.proof(id, chrono::Utc::now() - chrono::Duration::days(2));
    let root = Directory::root(&f.root).unwrap();
    let busy = scan::lock(&root.dir("cleanup", true).unwrap(), "lock", true).unwrap();
    f.enable_retention(1);
    let mut e = f.engine();
    for _ in 0..1000 {
        e.tick();
        if e.retention.last_check.is_some() {
            break;
        }
    }
    assert_eq!(e.retention.state, "check_failed");
    assert!(e.retention.last_job.is_none());
    assert!(e.retention.due.saturating_duration_since(Instant::now()) >= Duration::from_secs(50));
    assert!(f.root.join("runs").join(id.to_string()).exists());
    drop(busy);
    e.retention.due = Instant::now();
    for _ in 0..1000 {
        e.tick();
        if e.retention.last_job.is_some() && e.cleanup.is_none() {
            break;
        }
    }
    assert!(e.retention.last_job.is_some());
    assert!(!f.root.join("runs").join(id.to_string()).exists());
}

#[test]
fn full_preview_cache_does_not_spin_the_automatic_scheduler() {
    let f = Fixture::new();
    let (id, w) = f.run(false, true);
    drop(w);
    let mut e = f.engine();
    let p = preview(&mut e, PreviewMode::Manual(vec![id]));
    e.previews.clear();
    for _ in 0..16 {
        e.previews.insert(Uuid::new_v4(), p.clone());
    }
    f.enable_retention(1);
    e.retention.policy_at = Instant::now();
    e.tick();
    assert_eq!(e.retention.state, "check_failed");
    assert!(e.retention.due.saturating_duration_since(Instant::now()) >= Duration::from_secs(50));
    let last = e.retention.last_check.clone();
    for _ in 0..50 {
        e.tick();
    }
    assert_eq!(e.retention.last_check, last);
    assert!(e.waiting.is_empty());
}

fn complete_task(task: &mut cleanup::Task) {
    for _ in 0..10000 {
        if task.step().unwrap() {
            return;
        }
    }
    panic!("cleanup did not finish");
}
#[test]
fn quarantined_occupancy_is_measured_until_files_are_actually_removed() {
    let f = Fixture::new();
    f.enable_manual(true);
    let (id, w) = f.run(false, true);
    drop(w);
    let mut e = f.engine();
    let p = preview(&mut e, PreviewMode::Manual(vec![id]));
    let op = Uuid::new_v4();
    let mut task = cleanup::Task::create(
        &f.root,
        &f.workspace,
        f.current,
        f.config.handle(),
        &p,
        p.config_revision.as_deref().unwrap(),
        op,
    )
    .unwrap();
    task.crash = Some("after_quarantine");
    while task.step().is_ok() {}
    drop(task);
    drop(e);
    f.enable_manual(false);
    let mut e = f.engine();
    finish_scan(&mut e);
    let usage = e.query(Query::Usage { cursor: None }).unwrap();
    assert_eq!(usage["result"]["historyBytes"], 0);
    assert!(usage["result"]["pendingCleanup"]["bytes"].as_u64().unwrap() > 0);
    assert_eq!(usage["result"]["sharedManagement"]["status"], "complete");
    f.enable_manual(true);
    let mut task = cleanup::Task::resume(
        &f.root,
        &f.workspace,
        f.current,
        f.config.handle(),
        op,
        p.root_identity.unwrap(),
    )
    .unwrap();
    complete_task(&mut task);
    drop(task);
    e.query(Query::Refresh).unwrap();
    finish_scan(&mut e);
    assert_eq!(
        e.query(Query::Usage { cursor: None }).unwrap()["result"]["pendingCleanup"]["bytes"],
        0
    );
}
#[test]
fn cleanup_global_lock_replaced_targets_corrupt_policy_and_changed_revision_fail_closed() {
    let f = Fixture::new();
    f.enable_manual(true);
    let (id, w) = f.run(false, true);
    drop(w);
    let mut e = f.engine();
    let p = preview(&mut e, PreviewMode::Manual(vec![id]));
    let op = Uuid::new_v4();
    let mut task = cleanup::Task::create(
        &f.root,
        &f.workspace,
        f.current,
        f.config.handle(),
        &p,
        p.config_revision.as_deref().unwrap(),
        op,
    )
    .unwrap();
    assert!(matches!(
        cleanup::Task::create(
            &f.root,
            &f.workspace,
            f.current,
            f.config.handle(),
            &p,
            p.config_revision.as_deref().unwrap(),
            Uuid::new_v4()
        ),
        Err(Error::Busy)
    ));
    std::fs::write(f._tmp.path().join(".codex-web/config/config.json"), b"{").unwrap();
    complete_task(&mut task);
    assert!(f.root.join("runs").join(id.to_string()).exists());
    drop(task);
    f.enable_manual(true);
    let original = f.root.join("runs").join(id.to_string());
    let elsewhere = f._tmp.path().join("preserved-original");
    std::fs::rename(&original, &elsewhere).unwrap();
    std::fs::create_dir(&original).unwrap();
    let mut task = cleanup::Task::resume(
        &f.root,
        &f.workspace,
        f.current,
        f.config.handle(),
        op,
        p.root_identity.unwrap(),
    )
    .unwrap();
    assert!(task.step().is_err());
    task.record_failure();
    assert_eq!(task.job.value()["items"][0]["state"], "failed");
    assert!(elsewhere.join("meta.json").exists());
}
#[test]
fn confirmed_cleanup_is_idempotent_and_preserves_current_native_and_foreign_records() {
    let f = Fixture::new();
    let (id, w) = f.run(false, true);
    drop(w);
    let (other, w) = f.run(true, true);
    drop(w);
    let (active, _writer) = f.run(false, false);
    let native = f._tmp.path().join("home/sessions");
    std::fs::create_dir(&native).unwrap();
    std::fs::write(
        native.join("synthetic.jsonl"),
        b"synthetic native durable data",
    )
    .unwrap();
    let untouched = [
        f._tmp.path().join("home/archived_sessions/synthetic.jsonl"),
        f._tmp.path().join("legacy-observer/observer.sqlite"),
        f._tmp.path().join("project/source.txt"),
    ];
    for path in &untouched {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"synthetic data outside workbench history").unwrap();
    }
    let mut e = f.engine();
    let p = preview(&mut e, PreviewMode::Manual(vec![id, other, active]));
    let op = Uuid::new_v4();
    assert_eq!(
        e.query(Query::CreateJob {
            preview: p.preview_id,
            revision: p.config_revision.unwrap(),
            operation: op
        })
        .unwrap_err(),
        Error::Disabled
    );
    f.enable_manual(true);
    let p = preview(&mut e, PreviewMode::Manual(vec![id, other, active]));
    let q = || Query::CreateJob {
        preview: p.preview_id,
        revision: p.config_revision.clone().unwrap(),
        operation: op,
    };
    let started = e.query(q()).unwrap();
    assert_eq!(started["result"]["jobId"], op.to_string());
    assert!(e.query(q()).is_ok());
    for _ in 0..1000 {
        e.tick();
        if e.cleanup.is_none() {
            break;
        }
    }
    let job = e.query(Query::Job { id: op }).unwrap();
    assert_eq!(job["result"]["items"][0]["state"], "deleted");
    assert_eq!(job["result"]["items"][1]["state"], "skipped");
    assert!(!f.root.join("runs").join(id.to_string()).exists());
    assert!(f.root.join("runs").join(other.to_string()).exists());
    assert!(f.root.join("runs").join(active.to_string()).exists());
    assert!(e.query(q()).is_ok());
    assert_eq!(
        e.query(Query::CreateJob {
            preview: Uuid::new_v4(),
            revision: p.config_revision.unwrap(),
            operation: op
        })
        .unwrap_err(),
        Error::Stale
    );
    let root = Directory::root(&f.root).unwrap();
    assert!(known_removed(&root, &f.workspace, id));
    assert!(!known_removed(&root, &"f".repeat(64), id));
    assert_eq!(
        std::fs::read(native.join("synthetic.jsonl")).unwrap(),
        b"synthetic native durable data"
    );
    for path in untouched {
        assert_eq!(
            std::fs::read(path).unwrap(),
            b"synthetic data outside workbench history"
        );
    }
}
#[test]
fn intent_write_failure_never_moves_the_source_or_reports_a_successful_job() {
    let f = Fixture::new();
    f.enable_manual(true);
    let (id, writer) = f.run(false, true);
    drop(writer);
    let mut e = f.engine();
    let p = preview(&mut e, PreviewMode::Manual(vec![id]));
    let jobs = Directory::root(&f.root)
        .unwrap()
        .dir("cleanup", true)
        .unwrap()
        .dir("jobs", true)
        .unwrap();
    std::fs::set_permissions(&jobs.path, std::fs::Permissions::from_mode(0o500)).unwrap();
    let operation = Uuid::new_v4();
    let result = cleanup::Task::create(
        &f.root,
        &f.workspace,
        f.current,
        f.config.handle(),
        &p,
        p.config_revision.as_deref().unwrap(),
        operation,
    );
    std::fs::set_permissions(&jobs.path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(Error::Unavailable)));
    assert!(
        f.root
            .join("runs")
            .join(id.to_string())
            .join("meta.json")
            .exists()
    );
    assert!(
        cleanup::read_job(&Directory::root(&f.root).unwrap(), &f.workspace, operation).is_err()
    );
}
#[test]
fn cleanup_rechecks_inventory_and_lock_and_cancels_before_next_run() {
    let f = Fixture::new();
    f.enable_manual(true);
    let (first, w) = f.run(false, true);
    drop(w);
    let (second, w) = f.run(false, false);
    drop(w);
    let mut e = f.engine();
    let p = preview(&mut e, PreviewMode::Manual(vec![first, second]));
    let dir = Directory::root(&f.root)
        .unwrap()
        .dir("runs", false)
        .unwrap()
        .dir(&first.to_string(), false)
        .unwrap();
    dir.atomic("snapshot.json", b"synthetic replacement after preview")
        .unwrap();
    let mut task = cleanup::Task::create(
        &f.root,
        &f.workspace,
        f.current,
        f.config.handle(),
        &p,
        p.config_revision.as_deref().unwrap(),
        Uuid::new_v4(),
    )
    .unwrap();
    complete_task(&mut task);
    assert_eq!(task.job.value()["items"][0]["reason"], "preview_changed");
    assert_eq!(task.job.value()["items"][1]["state"], "deleted");
    drop(task);
    let (second, w) = f.run(false, true);
    drop(w);
    let p = preview(&mut e, PreviewMode::Manual(vec![first, second]));
    let mut task = cleanup::Task::create(
        &f.root,
        &f.workspace,
        f.current,
        f.config.handle(),
        &p,
        p.config_revision.as_deref().unwrap(),
        Uuid::new_v4(),
    )
    .unwrap();
    while !task.job.items[0].quarantined {
        task.step().unwrap();
    }
    task.cancel().unwrap();
    complete_task(&mut task);
    assert_eq!(task.job.value()["items"][0]["state"], "deleted");
    assert_eq!(task.job.value()["items"][1]["state"], "cancelled");
    assert!(f.root.join("runs").join(second.to_string()).exists());
}
#[test]
fn every_quarantine_and_removal_crash_point_resumes_only_its_durable_intent() {
    for point in [
        "before_rename",
        "after_rename",
        "after_quarantine",
        "before_delete",
        "during_delete",
        "before_metadata_delete",
        "after_metadata_delete",
        "after_directory_delete",
    ] {
        let f = Fixture::new();
        f.enable_manual(true);
        let (id, w) = f.run(false, true);
        drop(w);
        if point == "during_delete" {
            let blobs = Directory::root(&f.root)
                .unwrap()
                .dir("runs", false)
                .unwrap()
                .dir(&id.to_string(), false)
                .unwrap()
                .dir("blobs", false)
                .unwrap();
            for i in 0..300 {
                drop(blobs.open(&format!("{i:064x}"), true).unwrap());
            }
        }
        let mut e = f.engine();
        let p = preview(&mut e, PreviewMode::Manual(vec![id]));
        let op = Uuid::new_v4();
        let mut task = cleanup::Task::create(
            &f.root,
            &f.workspace,
            f.current,
            f.config.handle(),
            &p,
            p.config_revision.as_deref().unwrap(),
            op,
        )
        .unwrap();
        task.crash = Some(point);
        let mut crashed = false;
        for _ in 0..10000 {
            match task.step() {
                Err(_) => {
                    crashed = true;
                    break;
                }
                Ok(false) => {}
                Ok(true) => break,
            }
        }
        assert!(crashed, "{point}");
        drop(task);
        f.enable_manual(false);
        assert!(matches!(
            cleanup::Task::resume(
                &f.root,
                &f.workspace,
                f.current,
                f.config.handle(),
                op,
                p.root_identity.unwrap()
            ),
            Err(Error::Disabled)
        ));
        f.enable_manual(true);
        let mut task = cleanup::Task::resume(
            &f.root,
            &f.workspace,
            f.current,
            f.config.handle(),
            op,
            p.root_identity.unwrap(),
        )
        .unwrap();
        complete_task(&mut task);
        assert_eq!(task.job.value()["items"][0]["state"], "deleted", "{point}");
        assert!(!f.root.join("runs").join(id.to_string()).exists());
        assert!(
            !f.root
                .join("cleanup/trash")
                .join(op.to_string())
                .join(id.to_string())
                .exists()
        );
    }
}
fn finish_scan(e: &mut Engine) {
    for _ in 0..10000 {
        e.tick();
        if e.scan.as_ref().is_some_and(|s| s.info.state != "scanning") {
            return;
        }
    }
    panic!("scan failed to finish");
}
#[test]
fn directory_scan_continues_after_ten_thousand_unverifiable_entries() {
    let f = Fixture::new();
    for _ in 0..10010 {
        std::fs::create_dir(f.root.join("runs").join(Uuid::new_v4().to_string())).unwrap();
    }
    let (id, w) = f.run(false, true);
    drop(w);
    let mut e = f.engine();
    finish_scan(&mut e);
    let p = e.query(Query::Usage { cursor: None }).unwrap();
    assert_eq!(p["result"]["examinedEntries"], 10011);
    assert_eq!(p["result"]["unverifiedEntries"], 10010);
    assert_eq!(p["result"]["runs"][0]["runEpoch"], id.to_string());
    assert_eq!(p["result"]["state"], "partial");
}
fn preview(e: &mut Engine, mode: PreviewMode) -> model::Preview {
    let v = e.query(Query::Preview { mode }).unwrap();
    let id = serde_json::from_value::<Uuid>(v["result"]["previewId"].clone()).unwrap();
    for _ in 0..10000 {
        e.tick();
        let p = &e.previews[&id];
        if p.status == "ready" || p.status == "failed" {
            return p.clone();
        }
    }
    panic!("preview failed to finish");
}
#[test]
fn usage_measures_logical_bytes_pages_and_project_scope_with_stale_cursor_detection() {
    let f = Fixture::new();
    let mut ids = vec![];
    for _ in 0..24 {
        let (id, w) = f.run(false, true);
        drop(w);
        ids.push(id);
    }
    let (_, other) = f.run(true, true);
    drop(other);
    let (active, _writer) = f.run(false, false);
    let mut e = f.engine();
    finish_scan(&mut e);
    let page = e.query(Query::Usage { cursor: None }).unwrap();
    let p = &page["result"];
    assert_eq!(p["runCount"], 25);
    assert_eq!(p["state"], "complete");
    assert_eq!(p["runs"].as_array().unwrap().len(), 20);
    assert!(p["historyBytes"].as_u64().unwrap() > 0);
    assert!(p["activeBytes"].as_u64().unwrap() > 0);
    let cursor = p["nextCursor"].as_str().unwrap().to_owned();
    assert_eq!(
        e.query(Query::Usage {
            cursor: Some(cursor.clone())
        })
        .unwrap()["result"]["runs"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    let p = preview(&mut e, PreviewMode::Manual(vec![ids[0], active, f.current]));
    assert_eq!(p.status, "ready");
    assert!(!p.executable);
    assert!(p.items[0].eligible);
    assert_eq!(
        p.items[0].run.as_ref().unwrap().retention_reason.as_deref(),
        Some("ended_at_unknown")
    );
    assert_eq!(p.items[1].reason.as_deref(), Some("active_run"));
    assert!(!p.items[2].eligible);
    e.query(Query::Refresh).unwrap();
    finish_scan(&mut e);
    assert_eq!(
        e.query(Query::Usage {
            cursor: Some(cursor)
        })
        .unwrap_err(),
        Error::Stale
    );
}
#[test]
fn unreadable_links_unknown_files_and_other_project_never_become_deletion_candidates() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let (id, w) = f.run(false, false);
    drop(w);
    let (other, w) = f.run(true, true);
    drop(w);
    let native = f._tmp.path().join("native-history");
    std::fs::write(&native, "synthetic native history unchanged").unwrap();
    let run = f.root.join("runs").join(id.to_string());
    symlink(&native, run.join("snapshot.json")).unwrap();
    let mut e = f.engine();
    finish_scan(&mut e);
    let usage = e.query(Query::Usage { cursor: None }).unwrap();
    assert_eq!(usage["result"]["state"], "partial");
    assert_eq!(usage["result"]["runs"][0]["size"]["bytes"], Value::Null);
    let p = preview(&mut e, PreviewMode::Manual(vec![id, other]));
    assert!(p.items.iter().all(|i| !i.eligible));
    std::fs::remove_file(run.join("snapshot.json")).unwrap();
    let p = preview(&mut e, PreviewMode::Manual(vec![id]));
    assert!(p.items[0].eligible);
    assert_eq!(p.items[0].run.as_ref().unwrap().state, "unclean");
    std::fs::hard_link(&native, run.join("snapshot.json")).unwrap();
    let p = preview(&mut e, PreviewMode::Manual(vec![id]));
    assert!(!p.items[0].eligible);
    assert_eq!(
        std::fs::read_to_string(native).unwrap(),
        "synthetic native history unchanged"
    );
}
#[test]
fn chunked_blob_scan_yields_and_freezes_candidates_without_holding_writer_lock() {
    let f = Fixture::new();
    let (id, w) = f.run(false, true);
    drop(w);
    let root = Directory::root(&f.root).unwrap();
    let runs = root.dir("runs", false).unwrap();
    let dir = runs.dir(&id.to_string(), false).unwrap();
    let blobs = dir.dir("blobs", false).unwrap();
    for i in 0..300 {
        drop(blobs.open(&format!("{i:064x}"), true).unwrap());
    }
    let mut m = scan::Measure::begin(&runs, id, &f.workspace, f.current)
        .unwrap()
        .unwrap();
    assert!(!m.step(4).unwrap());
    drop(m);
    let mut e = f.engine();
    e.tick();
    let p = preview(&mut e, PreviewMode::Manual(vec![id]));
    assert!(p.items[0].eligible);
    assert!(scan::lock(&dir, "lock", false).is_ok());
    let (_, w) = f.run(false, true);
    drop(w);
    assert_eq!(p.items.len(), 1);
    assert_eq!(
        e.query(Query::Preview {
            mode: PreviewMode::Manual(vec![id; 101])
        })
        .unwrap_err(),
        Error::Invalid
    );
    std::fs::set_permissions(
        dir.path.join("meta.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(!preview(&mut e, PreviewMode::Manual(vec![id])).items[0].eligible);
}
#[test]
fn retention_preview_is_read_only_and_requires_bound_ended_evidence() {
    let f = Fixture::new();
    let (id, w) = f.run(false, true);
    drop(w);
    let root = Directory::root(&f.root).unwrap();
    let dir = root
        .dir("runs", false)
        .unwrap()
        .dir(&id.to_string(), false)
        .unwrap();
    let meta = super::super::journal::read_meta(&dir, id).unwrap();
    let digest = blake3::hash(&dir.read("meta.json", 4096).unwrap())
        .to_hex()
        .to_string();
    let proof = scan::Lifecycle {
        schema_version: 1,
        run_epoch: id,
        workspace_id: f.workspace.clone(),
        ended_at: "2026-01-02T00:00:00Z".into(),
        final_meta_digest: digest,
        saved_record_seq: meta.saved_record_seq,
    };
    dir.atomic("lifecycle.json", &serde_json::to_vec(&proof).unwrap())
        .unwrap();
    let mut e = f.engine();
    let p = preview(&mut e, PreviewMode::Retention(1));
    assert_eq!(p.items.len(), 1);
    assert!(!p.executable);
    assert!(dir.path.exists());
    dir.atomic("lifecycle.json", b"{}").unwrap();
    let p = preview(&mut e, PreviewMode::Retention(1));
    assert!(p.items.is_empty());
    assert_eq!(p.skipped_counts["lifecycle_invalid"], 1);
}
