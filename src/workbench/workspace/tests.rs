use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::process::Command;

fn fixture() -> (tempfile::TempDir, Reader) {
    let temp = tempfile::tempdir().unwrap();
    let reader = Reader {
        root: fs::Root::open(temp.path()).unwrap(),
        programs: process::Programs::discover(),
    };
    (temp, reader)
}
fn budget() -> Budget {
    Budget {
        deadline: Instant::now() + DEADLINE,
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}
fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Synthetic",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(status.status.success(), "synthetic Git setup failed");
}
#[test]
fn files_are_paged_and_cursor_is_bound_to_directory_snapshot() {
    let (dir, r) = fixture();
    for i in 0..205 {
        std::fs::write(dir.path().join(format!("file-{i:03}.txt")), b"safe").unwrap();
    }
    let first = r
        .query(
            Query::Files {
                path: "".into(),
                cursor: None,
            },
            &budget(),
        )
        .unwrap();
    assert_eq!(first["entries"].as_array().unwrap().len(), 200);
    let cursor = first["nextCursor"].as_str().unwrap().to_string();
    let next = r
        .query(
            Query::Files {
                path: "".into(),
                cursor: Some(cursor.clone()),
            },
            &budget(),
        )
        .unwrap();
    assert_eq!(next["entries"].as_array().unwrap().len(), 5);
    std::fs::write(dir.path().join("new.txt"), b"new").unwrap();
    assert_eq!(
        r.root.list("", Some(&cursor), &budget()).unwrap_err(),
        Fault("workspace_changed")
    );
    assert_eq!(
        r.root.list("", Some("bad"), &budget()).unwrap_err(),
        Fault("invalid_cursor")
    );
}
#[test]
fn actual_opens_reject_escape_links_hardlinks_devices_and_sensitive_files() {
    let (dir, r) = fixture();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("outside"), b"synthetic-outside").unwrap();
    symlink(outside.path(), dir.path().join("linked")).unwrap();
    symlink(outside.path().join("outside"), dir.path().join("direct")).unwrap();
    std::fs::hard_link(outside.path().join("outside"), dir.path().join("hard")).unwrap();
    for path in [
        "../outside",
        "/etc/passwd",
        "linked/outside",
        "direct",
        "hard",
        ".git/config",
        ".env",
        "dir/auth.json",
        "id_rsa",
        "key.pem",
        "a//b",
        "a/./b",
        "a\\b",
    ] {
        assert_eq!(
            r.root.read(path).unwrap_err(),
            Fault("forbidden_path"),
            "{path}"
        );
    }
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a"), b"safe").unwrap();
    std::fs::rename(dir.path().join("src"), dir.path().join("old")).unwrap();
    symlink(outside.path(), dir.path().join("src")).unwrap();
    assert_eq!(
        r.root.read("src/outside").unwrap_err(),
        Fault("forbidden_path")
    );
}
#[test]
fn root_is_pinned_and_file_limits_and_binary_are_explicit() {
    let (dir, r) = fixture();
    std::fs::write(
        dir.path().join("中文 -[a]\n.txt"),
        "你好\n<script>literal</script>\n",
    )
    .unwrap();
    assert!(r.root.read("中文 -[a]\n.txt").unwrap().contains("你好"));
    std::fs::write(dir.path().join("binary"), b"hello\0world").unwrap();
    assert_eq!(r.root.read("binary").unwrap_err(), Fault("binary_file"));
    std::fs::write(dir.path().join("large"), vec![b'a'; FILE_LIMIT + 1]).unwrap();
    assert_eq!(r.root.read("large").unwrap_err(), Fault("file_too_large"));
    assert_eq!(r.root.read("missing").unwrap_err(), Fault("not_found"));
    let moved = dir
        .path()
        .with_extension(uuid::Uuid::new_v4().simple().to_string());
    std::fs::rename(dir.path(), &moved).unwrap();
    std::fs::create_dir(dir.path()).unwrap();
    std::fs::write(dir.path().join("replacement"), b"wrong root").unwrap();
    assert!(r.root.read("中文 -[a]\n.txt").is_ok());
    assert_eq!(r.root.read("replacement").unwrap_err(), Fault("not_found"));
    std::fs::rename(&moved, dir.path().join("restore")).unwrap();
}
#[test]
fn rg_search_honors_ignores_and_safe_opens_without_shell_or_secret_results() {
    let (dir, r) = fixture();
    std::fs::write(dir.path().join(".ignore"), "ignored.txt\n").unwrap();
    for name in ["-中文 file[1].txt", "ignored.txt", ".env", "private.pem"] {
        std::fs::write(
            dir.path().join(name),
            "-needle 你好\n<script>literal</script>\n",
        )
        .unwrap();
    }
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("escape.txt"), "-needle outside").unwrap();
    symlink(outside.path(), dir.path().join("escape")).unwrap();
    let found = r.search("-needle", false, false, &budget()).unwrap();
    assert_eq!(found["hits"].as_array().unwrap().len(), 1, "{found}");
    assert_eq!(found["hits"][0]["path"], "-中文 file[1].txt");
    assert_eq!(found["hits"][0]["line"], 1);
    assert!(!found["truncated"].as_bool().unwrap());
    assert_eq!(
        r.search("[", true, false, &budget()).unwrap_err(),
        Fault("invalid_regex")
    );
    let mut missing = r;
    missing.programs.rg = None;
    assert_eq!(
        missing
            .search("hello", false, false, &budget())
            .unwrap_err(),
        Fault("rg_unavailable")
    );
}
#[test]
fn search_budget_reports_partial_results() {
    let (dir, r) = fixture();
    std::fs::write(dir.path().join("hits.txt"), "needle\n".repeat(205)).unwrap();
    let found = r.search("needle", false, true, &budget()).unwrap();
    assert_eq!(found["hits"].as_array().unwrap().len(), 200);
    assert_eq!(found["limitReason"], "hit_limit");
}
#[test]
fn git_nested_project_and_literal_paths_staging_log_and_no_external_execution() {
    let (dir, _) = fixture();
    let root = dir.path();
    git(root, &["init", "-q"]);
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("outer.txt"), "outer\n").unwrap();
    std::fs::write(root.join("sub/-中文 [x]\n.txt"), "old\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "Synthetic first"]);
    std::fs::write(root.join("outer.txt"), "outer-secret-change\n").unwrap();
    std::fs::write(root.join("sub/-中文 [x]\n.txt"), "staged\n").unwrap();
    git(root, &["add", "--", "sub/-中文 [x]\n.txt"]);
    std::fs::write(root.join("sub/-中文 [x]\n.txt"), "working\n").unwrap();
    std::fs::write(root.join("sub/untracked.txt"), "untracked\n").unwrap();
    std::fs::write(root.join("sub/.env"), "SECRET_FIXTURE\n").unwrap();
    let marker = root.join("executed");
    let script = root.join("extension.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    git(root, &["config", "diff.external", script.to_str().unwrap()]);
    git(
        root,
        &["config", "core.fsmonitor", script.to_str().unwrap()],
    );
    let r = Reader {
        root: fs::Root::open(&root.join("sub")).unwrap(),
        programs: process::Programs::discover(),
    };
    let status = r.git_status(&budget()).unwrap();
    let text = status.to_string();
    assert!(!text.contains("outer"));
    assert!(!text.contains(".env"));
    assert!(text.contains("untracked.txt"));
    let staged = r.git_diff("-中文 [x]\n.txt", true, &budget()).unwrap();
    assert!(
        staged["patch"].as_str().unwrap().contains("+staged"),
        "{staged}"
    );
    assert!(staged["patch"].as_str().unwrap().contains("-old"));
    let working = r.git_diff("-中文 [x]\n.txt", false, &budget()).unwrap();
    assert!(working["patch"].as_str().unwrap().contains("+working"));
    assert!(working["patch"].as_str().unwrap().contains("-staged"));
    let untracked = r.git_diff("untracked.txt", false, &budget()).unwrap();
    assert!(untracked["patch"].as_str().unwrap().contains("+untracked"));
    let log = r.git_log(None, &budget()).unwrap();
    assert_eq!(log["commits"][0]["subject"], "Synthetic first");
    assert!(!marker.exists());
}
#[test]
fn git_absent_unborn_binary_delete_and_log_cursor_conflict_are_explicit() {
    let (dir, mut r) = fixture();
    assert_eq!(
        r.git_status(&budget()).unwrap_err(),
        Fault("not_git_repository")
    );
    r.programs.git = None;
    assert_eq!(
        r.git_status(&budget()).unwrap_err(),
        Fault("git_unavailable")
    );
    r.programs = process::Programs::discover();
    git(dir.path(), &["init", "-q"]);
    assert_eq!(r.git_log(None, &budget()).unwrap()["commits"], json!([]));
    std::fs::write(dir.path().join("file"), b"before\n").unwrap();
    git(dir.path(), &["add", "file"]);
    assert!(
        r.git_diff("file", true, &budget()).unwrap()["patch"]
            .as_str()
            .unwrap()
            .contains("+before")
    );
    git(dir.path(), &["commit", "-qm", "initial"]);
    std::fs::remove_file(dir.path().join("file")).unwrap();
    assert!(
        r.git_diff("file", false, &budget()).unwrap()["patch"]
            .as_str()
            .unwrap()
            .contains("-before")
    );
    std::fs::write(dir.path().join("binary"), b"binary\0").unwrap();
    assert_eq!(
        r.git_diff("binary", false, &budget()).unwrap_err(),
        Fault("binary_file")
    );
    for n in 0..21 {
        std::fs::write(dir.path().join("file"), format!("{n}")).unwrap();
        git(dir.path(), &["add", "file"]);
        git(dir.path(), &["commit", "-qm", "fixture"]);
    }
    let page = r.git_log(None, &budget()).unwrap();
    let cursor = page["nextCursor"].as_str().unwrap();
    assert_eq!(
        r.git_log(Some(cursor), &budget()).unwrap()["commits"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    std::fs::write(dir.path().join("file"), b"last").unwrap();
    git(dir.path(), &["add", "file"]);
    git(dir.path(), &["commit", "-qm", "last"]);
    assert_eq!(
        r.git_log(Some(cursor), &budget()).unwrap_err(),
        Fault("workspace_changed")
    );
}
#[test]
fn cancelled_and_timed_out_processes_are_killed_and_bounded() {
    let (_dir, r) = fixture();
    let mut b = budget();
    b.deadline = Instant::now() + Duration::from_millis(50);
    let started = Instant::now();
    assert!(matches!(
        process::run(
            &r.root,
            Path::new("/bin/sleep"),
            &["5".into()],
            &[],
            &b,
            1024
        ),
        Err(Fault("workspace_timeout"))
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    let b = budget();
    let cancel = b.cancelled.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        cancel.store(true, Ordering::Relaxed);
    });
    assert!(matches!(
        process::run(
            &r.root,
            Path::new("/bin/sleep"),
            &["5".into()],
            &[],
            &b,
            1024
        ),
        Err(Fault("cancelled"))
    ));
    handle.join().unwrap();
    let output = process::run(
        &r.root,
        Path::new("/usr/bin/yes"),
        &[],
        &[],
        &budget(),
        4096,
    )
    .unwrap();
    assert!(output.truncated);
    assert_eq!(output.bytes.len(), 4096);
}

#[tokio::test]
async fn dropping_query_cancels_worker_and_bounded_queue_does_not_block_async_runtime() {
    let (dir, mut reader) = fixture();
    std::fs::write(dir.path().join("safe"), "still readable").unwrap();
    let script = dir.path().join("slow-program");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s' \"$$\" > started\nsleep 10\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    reader.programs.rg = Some(script);
    let handle = Handle::from_reader(dir.path(), reader).unwrap();
    let client = handle.clone();
    let query_requested_at = Instant::now();
    let mut slow = tokio::spawn(async move {
        client
            .query(Query::Search {
                text: "query".into(),
                regex: false,
                case_sensitive: false,
            })
            .await
    });
    // Startup is fixture preparation, not the cancellation latency assertion.
    // Use the real query deadline; cancellation below must still finish in 1s.
    tokio::time::timeout(DEADLINE, async {
        while !dir.path().join("started").exists() {
            if slow.is_finished() {
                panic!(
                    "synthetic search ended before readiness: {:?}",
                    (&mut slow).await
                );
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("synthetic search did not start within its query deadline");
    let mut pending = Vec::new();
    for _ in 0..8 {
        let (reply, rx) = oneshot::channel();
        handle
            .tx
            .try_send(Work {
                query: Query::File {
                    path: "safe".into(),
                },
                budget: budget(),
                reply,
            })
            .unwrap();
        pending.push(rx);
    }
    assert_eq!(
        handle
            .query(Query::File {
                path: "safe".into()
            })
            .await
            .unwrap_err(),
        Fault("workspace_busy")
    );
    // Natural expiry must remain beyond the cancellation assertion window.
    // Otherwise a near-deadline request could pass even if cancellation broke.
    assert!(
        query_requested_at.elapsed() < DEADLINE - Duration::from_secs(2),
        "fixture preparation left too little time to distinguish cancellation from natural expiry"
    );
    let start = Instant::now();
    slow.abort();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), slow)
            .await
            .expect("query cancellation did not complete within 1s")
            .unwrap_err()
            .is_cancelled()
    );
    for reply in pending {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), reply)
                .await
                .unwrap()
                .unwrap()
                .unwrap()["text"],
            "still readable"
        );
    }
    assert!(start.elapsed() < Duration::from_secs(1));
    let pid: i32 = std::fs::read_to_string(dir.path().join("started"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "query child must have exited"
    );
}

#[test]
fn concurrent_path_replacement_never_returns_root_external_contents() {
    let (dir, reader) = fixture();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("value"), "synthetic external").unwrap();
    std::fs::create_dir(dir.path().join("slot")).unwrap();
    std::fs::write(dir.path().join("slot/value"), "inside").unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let stop = running.clone();
    let root = dir.path().to_path_buf();
    let external = outside.path().to_path_buf();
    let swapping = std::thread::spawn(move || {
        while stop.load(Ordering::Relaxed) {
            std::fs::rename(root.join("slot"), root.join("held")).unwrap();
            symlink(&external, root.join("slot")).unwrap();
            std::thread::yield_now();
            std::fs::remove_file(root.join("slot")).unwrap();
            std::fs::rename(root.join("held"), root.join("slot")).unwrap();
        }
    });
    for _ in 0..1000 {
        if let Ok(text) = reader.root.read("slot/value") {
            assert_eq!(text, "inside");
        }
    }
    running.store(false, Ordering::Relaxed);
    swapping.join().unwrap();
}
