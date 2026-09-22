use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}
fn group_members(group: i32) -> Vec<i32> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,pgid="])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            (fields.next()?.parse::<i32>().ok()? == group).then_some(pid)
        })
        .collect()
}
// A failing regression must also clean up its own isolated browser group.
struct ChromeGroup(i32);
impl Drop for ChromeGroup {
    fn drop(&mut self) {
        if !group_members(self.0).is_empty() {
            unsafe { libc::kill(-self.0, libc::SIGKILL) };
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed Chrome; synthetic about:blank, no user browser"]
async fn dropped_probe_cleans_chrome_after_ready_during_launch_and_when_unresponsive() {
    let mut unrelated = Command::new("/bin/sleep")
        .arg("120")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for mode in ["ready", "eof", "launching", "unresponsive"] {
        let directory = tempfile::tempdir().unwrap();
        let wrapper = directory.path().join("chrome-fixture");
        let pid_file = directory.path().join("chrome.pid");
        let ready_file = directory.path().join("ready");
        std::fs::write(
            &wrapper,
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$PROBE_PID_FILE\"\nif [ \"$PROBE_MODE\" = launching ]; then sleep 1; fi\nexec \"$PROBE_REAL_CHROME\" \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = Command::new("node");
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/web/e2e/browser-lifecycle-fixture.cjs"
            ))
            .env("HOME", directory.path())
            .env("USERPROFILE", directory.path())
            .env("CODEX_HOME", directory.path().join("native"))
            .env("PROBE_MODE", mode)
            .env("PROBE_PID_FILE", &pid_file)
            .env("PROBE_READY_FILE", &ready_file)
            .env("WORKBENCH_TEST_CHROME", &wrapper)
            .env(
                "PROBE_REAL_CHROME",
                std::env::var_os("WORKBENCH_TEST_CHROME").unwrap_or_else(|| {
                    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into()
                }),
            )
            .env("DEBUG", "")
            .env("PWDEBUG", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut probe = ProbeProcess::spawn(&mut command).unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let ready = pid_file.exists() && (mode == "launching" || ready_file.exists());
                if ready {
                    break;
                }
                assert!(
                    probe.try_wait().unwrap().is_none(),
                    "fixture exited before {mode}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let cleanup = ChromeGroup(pid);
        assert!(alive(pid));
        let members = group_members(pid);
        if mode != "launching" {
            assert!(
                members.len() > 1,
                "fixture did not exercise Chrome children"
            );
        }
        if mode == "unresponsive" {
            assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        }
        let at = Instant::now();
        if mode == "eof" {
            probe.stdin.take();
            tokio::time::timeout(Duration::from_secs(8), probe.wait())
                .await
                .unwrap()
                .unwrap();
        }
        drop(probe);
        let gone = tokio::time::timeout(Duration::from_secs(6), async {
            while !group_members(pid).is_empty() || members.iter().any(|pid| alive(*pid)) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok();
        assert!(gone, "dropping {mode} probe left Chrome or a helper alive");
        assert!(at.elapsed() < Duration::from_secs(20));
        assert!(unrelated.try_wait().unwrap().is_none());
        drop(cleanup);
    }
    unrelated.kill().await.unwrap();
    unrelated.wait().await.unwrap();
}
