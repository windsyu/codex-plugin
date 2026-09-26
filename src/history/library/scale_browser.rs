//! Opt-in, real binary + system Chrome cold/warm UI measurements.
use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

#[tokio::test]
#[ignore = "requires built codex-view and Chrome; writes >2GiB synthetic history in isolated temp HOME"]
async fn binary_large_history_chrome_cold_and_warm_metrics() {
    let temp = tempfile::tempdir().unwrap();
    let count = std::env::var("WORKBENCH_SCALE_SESSIONS")
        .ok()
        .map(|v| v.parse::<usize>().unwrap())
        .unwrap_or(10_000);
    let (home, manifest) = super::scale_tests::generate_fixture(temp.path(), count);
    println!("{}", json!({"stage":"fixture","manifest":manifest}));
    let cli = temp.path().join("unused-cli");
    fs::write(
        &cli,
        "#!/bin/sh\nprintf invocation >> \"$CODEX_HOME/invocations\"\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            if self.0.try_wait().unwrap().is_none() {
                unsafe {
                    libc::kill(self.0.id() as i32, libc::SIGTERM);
                }
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    if self.0.try_wait().ok().flatten().is_some() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    let entry_path = temp.path().join("entry.json");
    let data = temp.path().join("history");
    let binary = std::env::var_os("WORKBENCH_TEST_LAUNCHER")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/codex-view"));
    let mut child = Child(
        Command::new(binary)
            .current_dir(temp.path())
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("CODEX_HOME", &home)
            .args(["--no-open", "--codex-bin"])
            .arg(cli)
            .arg("--data-dir")
            .arg(&data)
            .arg("--entry-file")
            .arg(&entry_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let entry = loop {
        if let Ok(entry) = crate::workbench::launch::read_entry(&entry_path) {
            break entry;
        }
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let mut command = tokio::process::Command::new("node");
    command
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r6-scale-probe.cjs"))
        .env("WORKBENCH_PROBE_URL", &entry.url)
        .env("WORKBENCH_PROBE_SESSIONS", count.to_string())
        .env("WORKBENCH_PROBE_PID", child.0.id().to_string())
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("CODEX_HOME", &home)
        .env("DEBUG", "")
        .env("PWDEBUG", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null());
    let mut probe = crate::workbench::probe_process::ProbeProcess::spawn(&mut command).unwrap();
    let _live = probe.stdin.take();
    assert!(
        tokio::time::timeout(Duration::from_secs(490), probe.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!home.join("invocations").exists());
    let bytes: u64 = walkdir::WalkDir::new(data.join("library"))
        .into_iter()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.metadata().unwrap().len())
        .sum();
    assert!(bytes <= 2 * 1024 * 1024 * 1024);
    let stopped = Instant::now();
    drop(child);
    println!(
        "{}",
        json!({"stage":"shutdown","elapsedMs":stopped.elapsed().as_secs_f64()*1000.,"catalogDirectoryBytes":bytes,"cliInvocations":0})
    );
    assert!(!entry_path.exists());
}
