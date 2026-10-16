use super::*;
use std::os::unix::{
    fs::{PermissionsExt, symlink},
    process::CommandExt,
};
use std::os::{fd::AsRawFd, unix::fs::FileExt};
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
fn fd_pressure(target: i32) -> Vec<std::fs::File> {
    let mut files = Vec::new();
    loop {
        let file = std::fs::File::open("/dev/null").unwrap();
        let fd = file.as_raw_fd();
        files.push(file);
        if fd >= target {
            return files;
        }
    }
}
// Only bounded test summaries / a few diagnostic events are captured here.
// A high-volume diagnostic worker would need concurrent pipe draining.
fn isolated_test(name: &str, key: &str, value: &str, diagnostics: bool) -> std::process::Output {
    // Hunk-like text in TMPDIR must never escape through stripped Git headers.
    let isolated = tempfile::Builder::new()
        .prefix("workspace @@ -private- ")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", isolated.path())
        .env("USERPROFILE", isolated.path())
        .env("CODEX_HOME", isolated.path().join("codex"))
        .env("TMPDIR", isolated.path())
        .env(key, value)
        .env(
            "CODEX_WORKSPACE_DIAGNOSTICS",
            if diagnostics { "1" } else { "0" },
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            panic!("isolated workspace test timed out: {name}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
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
    assert_eq!(staged["patch"], "@@ -1 +1 @@\n-old\n+staged\n");
    assert_eq!(
        staged["revision"],
        blake3::hash(b"old\n\0staged\n").to_hex().as_str()
    );
    let working = r.git_diff("-中文 [x]\n.txt", false, &budget()).unwrap();
    assert!(
        working["patch"].as_str().unwrap().contains("+working"),
        "{working}"
    );
    assert!(
        working["patch"].as_str().unwrap().contains("-staged"),
        "{working}"
    );
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

// Re-exec rather than reserving/overwriting descriptors in the parallel test
// harness. Opening /dev/null only occupies descriptors owned by this child.
#[test]
fn process_fd_pressure_preserves_snapshots_and_exec_failure() {
    const CASE: &str = "WORKSPACE_FD_PRESSURE_CASE";
    if let Ok(case) = std::env::var(CASE) {
        let (dir, r) = fixture();
        let files = [
            process::snapshot(b"old\n").unwrap(),
            process::snapshot(b"staged\n").unwrap(),
        ];
        let target: i32 = case.parse().unwrap();
        let _pressure = fd_pressure(target);
        let result = process::run(
            &r.root,
            &dir.path().join("missing-executable"),
            &[],
            &files,
            &budget(),
            1024,
        );
        let mut bytes = [0; 7];
        let n = files[1].read_at(&mut bytes, 0).unwrap();
        let after_intact = n == 7 && &bytes == b"staged\n";
        let mut old = [0; 4];
        let before_intact = files[0].read_at(&mut old, 0).unwrap() == 4 && &old == b"old\n";
        assert!(
            matches!(result, Err(Fault("program_unavailable"))),
            "exec failure lost with pressure through fd {target}; code={:?}, snapshot_lengths={:?}, snapshots_intact={:?}",
            result.as_ref().ok().and_then(|out| out.code),
            files
                .iter()
                .map(|f| f.metadata().unwrap().len())
                .collect::<Vec<_>>(),
            [before_intact, after_intact],
        );
        assert!(
            before_intact && after_intact,
            "exec failure changed snapshot"
        );
        // The same live descriptors must also survive a successful Git exec,
        // including layouts where Command's internal pipes are near 64/65.
        let out = process::run(
            &r.root,
            r.programs.git.as_ref().unwrap(),
            &[
                "diff".into(),
                "--no-index".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--".into(),
                process::snapshot_path(&files[0]),
                process::snapshot_path(&files[1]),
            ],
            &files,
            &budget(),
            1024,
        )
        .unwrap();
        assert_eq!(out.code, Some(1));
        let patch = String::from_utf8(out.bytes).unwrap();
        assert!(patch.contains("+staged") && patch.contains("-old"));
        return;
    }
    let name =
        "workbench::workspace::tests::process_fd_pressure_preserves_snapshots_and_exec_failure";
    let mut failures = Vec::new();
    for target in 54..=68 {
        let output = isolated_test(name, CASE, &target.to_string(), false);
        if !output.status.success() {
            failures.push(format!(
                "isolated FD pressure case {target}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn snapshots_start_at_zero_and_remain_cloexec_in_parent() {
    let (_dir, r) = fixture();
    let file = process::snapshot(b"synthetic\n").unwrap();
    let before = process::fd_metadata(&file);
    assert_eq!(before["bytes"], 10);
    assert_eq!(before["offset"], 0);
    assert_eq!(
        before["descriptorFlags"].as_i64().unwrap() & i64::from(libc::FD_CLOEXEC),
        i64::from(libc::FD_CLOEXEC)
    );
    let output = process::run(
        &r.root,
        Path::new("/bin/cat"),
        &[process::snapshot_path(&file)],
        std::slice::from_ref(&file),
        &budget(),
        1024,
    )
    .unwrap();
    assert_eq!(output.bytes, b"synthetic\n");
    let after = process::fd_metadata(&file);
    assert_eq!(before["inode"], after["inode"]);
    assert_eq!(before["bytes"], after["bytes"]);
    assert_eq!(before["descriptorFlags"], after["descriptorFlags"]);
    // macOS /dev/fd opens duplicate the same open file description. A second
    // read would share EOF; diagnostic probes must not read/rewind the input.
    #[cfg(target_os = "macos")]
    assert_eq!(after["offset"], 10);
    let status = Command::new("/bin/sh")
        .env_clear()
        .args([
            "-c",
            &format!("test ! -e {}", process::snapshot_path(&file)),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "unrelated process inherited a snapshot");
}

#[test]
fn search_maps_dynamic_fds_across_batches_without_rewriting_literal_query() {
    let (dir, r) = fixture();
    for n in 0..19 {
        std::fs::write(dir.path().join(format!("file-{n:02}")), "/dev/fd/64\n").unwrap();
    }
    let out = r.search("/dev/fd/64", false, true, &budget()).unwrap();
    let hits = out["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 19);
    // rg may emit parallel file matches in any order; path ownership is the
    // contract, not completion order.
    let mut paths: Vec<_> = hits
        .iter()
        .map(|hit| hit["path"].as_str().unwrap())
        .collect();
    paths.sort_unstable();
    for (n, path) in paths.iter().enumerate() {
        assert_eq!(*path, format!("file-{n:02}"));
    }
    for hit in hits {
        assert_eq!(hit["text"], "/dev/fd/64");
    }
}

#[test]
fn git_staged_diff_under_fd_pressure_and_concurrent_processes() {
    const CASE: &str = "WORKSPACE_CONCURRENT_FD_CASE";
    if std::env::var_os(CASE).is_some() {
        let (dir, r) = fixture();
        git(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("file"), b"old\n").unwrap();
        git(dir.path(), &["add", "file"]);
        git(dir.path(), &["commit", "-qm", "synthetic"]);
        std::fs::write(dir.path().join("file"), b"staged\n").unwrap();
        git(dir.path(), &["add", "file"]);
        std::fs::write(dir.path().join("file"), b"working\n").unwrap();
        let _pressure = fd_pressure(192);
        let barrier = std::sync::Barrier::new(5);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let r = &r;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    for n in 0..16 {
                        let staged = n % 2 == 0;
                        let out = r.git_diff("file", staged, &budget()).unwrap();
                        let expected = if staged {
                            "old\n\0staged\n"
                        } else {
                            "staged\n\0working\n"
                        };
                        assert_eq!(
                            out["revision"],
                            blake3::hash(expected.as_bytes()).to_hex().as_str()
                        );
                        let patch = out["patch"].as_str().unwrap();
                        assert!(patch.contains(if staged { "+staged" } else { "+working" }));
                        assert!(patch.contains(if staged { "-old" } else { "-staged" }));
                        assert_eq!(out["truncated"], false);
                    }
                });
            }
            barrier.wait();
            for _ in 0..64 {
                let file = process::snapshot(b"unrelated\n").unwrap();
                let path = process::snapshot_path(&file);
                let out = process::run(
                    &r.root,
                    Path::new("/bin/cat"),
                    &[path],
                    &[file],
                    &budget(),
                    1024,
                )
                .unwrap();
                assert_eq!(out.code, Some(0));
                assert_eq!(out.bytes, b"unrelated\n");
            }
        });
        assert_eq!(
            std::fs::read(dir.path().join("file")).unwrap(),
            b"working\n"
        );
        return;
    }
    let name =
        "workbench::workspace::tests::git_staged_diff_under_fd_pressure_and_concurrent_processes";
    let out = isolated_test(name, CASE, "1", false);
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn git_original_staged_diff_under_fd_layout_matrix() {
    const CASE: &str = "WORKSPACE_ORIGINAL_DIFF_CASE";
    if let Ok(case) = std::env::var(CASE) {
        let (target, threads) = case.split_once(':').unwrap();
        let target: i32 = target.parse().unwrap();
        let threads: usize = threads.parse().unwrap();
        let _pressure = (target > 0).then(|| fd_pressure(target));
        let barrier = std::sync::Barrier::new(threads);
        std::thread::scope(|scope| {
            for _ in 0..threads {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..2 {
                        // Reuse every original assertion, including literal/nested
                        // paths, staged vs working content and no external exec.
                        let original = git_nested_project_and_literal_paths_staging_log_and_no_external_execution;
                        original();
                    }
                });
            }
        });
        return;
    }
    let name = "workbench::workspace::tests::git_original_staged_diff_under_fd_layout_matrix";
    let mut failures = Vec::new();
    for target in [0, 54, 57, 58, 63, 64, 65, 68, 128, 192] {
        for threads in [1, 4] {
            let case = format!("{target}:{threads}");
            let output = isolated_test(name, CASE, &case, false);
            if !output.status.success() {
                failures.push(format!(
                    "original diff layout {case}: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn git_diff_isolated_from_dev_fd_stat_races() {
    const CASE: &str = "WORKSPACE_FD_STAT_CASE";
    if std::env::var_os(CASE).is_some() {
        // Keep these descriptors valid in this process while Git uses its own
        // FD table. Merely stat'ing the same /dev/fd number on macOS can race
        // with the child's stat; no contents or offsets are changed here.
        let _pressure = fd_pressure(192);
        let stop = AtomicBool::new(false);
        let ready = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                ready.wait();
                while !stop.load(Ordering::Relaxed) {
                    let _ = std::fs::symlink_metadata("/dev/fd/64");
                    let _ = std::fs::symlink_metadata("/dev/fd/65");
                }
            });
            ready.wait();
            let result = std::panic::catch_unwind(|| {
                for _ in 0..20 {
                    let original =
                        git_nested_project_and_literal_paths_staging_log_and_no_external_execution;
                    original();
                }
            });
            stop.store(true, Ordering::Relaxed);
            result.unwrap();
        });
        return;
    }
    let name = "workbench::workspace::tests::git_diff_isolated_from_dev_fd_stat_races";
    let output = isolated_test(name, CASE, "1", false);
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn private_diff_snapshots_are_exclusive_owner_only_and_scoped() {
    let outer = tempfile::tempdir().unwrap();
    let paths;
    {
        let directory = process::snapshot_directory().unwrap();
        assert_eq!(
            directory.path().metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );
        paths = [
            directory.path().join("before"),
            directory.path().join("after"),
        ];
        for path in &paths {
            let file = process::snapshot_at(path, b"synthetic\n").unwrap();
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(process::fd_metadata(&file)["offset"], 0);
            assert!(process::snapshot_at(path, b"overwrite").is_err());
            assert_eq!(std::fs::read(path).unwrap(), b"synthetic\n");
        }
        let outside = outer.path().join("outside");
        std::fs::write(&outside, b"outside\n").unwrap();
        let link = directory.path().join("link");
        symlink(&outside, &link).unwrap();
        assert!(process::snapshot_at(&link, b"overwrite").is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"outside\n");
    }
    assert!(paths.iter().all(|path| !path.exists()));
}

#[test]
fn process_diagnostics_are_structured_and_do_not_disclose_content_or_stderr() {
    const CASE: &str = "WORKSPACE_DIAGNOSTIC_CASE";
    if std::env::var_os(CASE).is_some() {
        let (dir, r) = fixture();
        let file = process::snapshot(b"PRIVATE_SNAPSHOT_SENTINEL\n").unwrap();
        let out = process::run(&r.root, Path::new("/bin/sh"), &["-c".into(), "printf 'PRIVATE_STDERR_SENTINEL bad file descriptor /private/secret-path\\n' >&2; exit 42".into()], std::slice::from_ref(&file), &budget(), 1024).unwrap();
        assert_eq!(out.code, Some(42));
        assert!(matches!(
            process::run(
                &r.root,
                &dir.path().join("missing-executable"),
                &[],
                std::slice::from_ref(&file),
                &budget(),
                1024
            ),
            Err(Fault("program_unavailable"))
        ));
        return;
    }
    let name = "workbench::workspace::tests::process_diagnostics_are_structured_and_do_not_disclose_content_or_stderr";
    let out = isolated_test(name, CASE, "1", true);
    assert!(out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(!stderr.contains("PRIVATE_"));
    assert!(!stderr.contains("secret-path"));
    assert!(!stderr.contains("printf"));
    let events: Vec<Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 5);
    assert_eq!(events[0]["expectedBytes"], 26);
    assert_eq!(events[1]["inputs"][0]["offset"], 0);
    assert_eq!(events[1]["inputs"][0]["fd"], events[2]["inputs"][0]["fd"]);
    assert_eq!(events[2]["exitCode"], 42);
    assert!(events[2]["childPid"].as_u64().unwrap() > 0);
    assert!(events[2]["stderrBytes"].as_u64().unwrap() > 0);
    assert_eq!(
        events[2]["stderrCategories"],
        json!(["bad_file_descriptor"])
    );
    assert_eq!(events[2]["stderrComplete"], true);
    assert_eq!(events[2]["stderrLimited"], false);
    assert_eq!(events[4]["spawnErrno"], libc::ENOENT);
    assert!(events[4]["childPid"].is_null());
    assert!(events[4]["exitCode"].is_null());
    assert_eq!(events[4]["error"], "program_unavailable");
}

#[test]
fn process_stderr_is_drained_and_keeps_its_existing_limit() {
    let (_dir, r) = fixture();
    let out = process::run(
        &r.root,
        Path::new("/bin/sh"),
        &["-c".into(), "head -c 65536 /dev/zero >&2; exit 7".into()],
        &[],
        &budget(),
        1024,
    )
    .unwrap();
    assert_eq!(out.code, Some(7));
    assert!(out.bytes.is_empty());
    assert!(!out.truncated);
    assert!(matches!(
        process::run(
            &r.root,
            Path::new("/bin/sh"),
            &["-c".into(), "head -c 65537 /dev/zero >&2".into()],
            &[],
            &budget(),
            1024
        ),
        Err(Fault("program_output_limit"))
    ));
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
