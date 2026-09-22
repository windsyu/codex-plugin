//! Real-provider acceptance through the shipped launcher, never the user's cwd.
use super::Profile;
use anyhow::{Result, ensure};
use base64::Engine;
use codex_local_observer::workbench::launch::read_entry;
use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

struct Launcher(tokio::process::Child);
impl Drop for Launcher {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        if let Some(pid) = self.0.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            let until = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < until {
                if self.0.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.0.start_kill();
        }
    }
}

fn native_counts(home: &Path) -> (usize, usize, usize) {
    fn walk(path: &Path, totals: &mut (usize, usize, usize)) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, totals);
            } else if path.extension().is_some_and(|s| s == "jsonl") {
                let Ok(text) = std::fs::read_to_string(path) else {
                    continue;
                };
                for line in text.lines() {
                    let Ok(value) = serde_json::from_str::<Value>(line) else {
                        continue;
                    };
                    match value.pointer("/payload/type").and_then(Value::as_str) {
                        Some("turn_aborted") => totals.0 += 1,
                        Some("task_complete") => totals.1 += 1,
                        Some("message")
                            if value.pointer("/payload/role").and_then(Value::as_str)
                                == Some("user") =>
                        {
                            totals.2 += value
                                .pointer("/payload/content")
                                .and_then(Value::as_array)
                                .map_or(0, |items| {
                                    items.iter().filter(|v| v["type"] == "input_image").count()
                                });
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    let mut counts = (0, 0, 0);
    walk(&home.join("sessions"), &mut counts);
    counts
}

pub(super) async fn run(profile: &Profile, controls: bool) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let home = directory.path().join("native");
    let workspace = directory.path().join("project");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&workspace)?;
    std::fs::write(workspace.join("R5-check.txt"), "R5_TOOL_FILE_7391\n")?;
    // A generated 128x128 RGB fixture, red left half and blue right half.
    let image = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAIAAAACACAIAAABMXPacAAABjUlEQVR4nO3RwQnAABCEwOu/6aSIPIYlgn8F77mbRvs/o/0N8AkNWEb7G+ATGjCN9jfAJzRgGe1vgE9owDTa3wCf0IBltL8BPqEB02h/A3xCA5bR/gb4hAZMo/0N8AkNWEb7G+ATGjCN9jfAJzRgGe1vgE9owDTa3wCf0IBltL8BPqEB02h/A3xCA5bR/gb4hAZMo/0N8AkNWEb7G+ATGjCN9jfAJzRgGe1vgE9owDTa3wCf0IBltL8BPqEB02h/A3xCA5bR/gb4hAZMo/0N8AkNWEb7G+ATGjCN9jfAJzRgGe1vgE9owDTa3wCf0IBltL8BPqEB02h/A3xCA5bR/gb4hAZMo/0N8AkNWEb7G+ATGjCN9jfAJzRgGe1vgE9owDTa3wCf0IBltL8BPqEB02h/A3xCA5bR/gb4hAZMo/0N8AkNWEb7G+ATGjCN9jfAJzRgGe1vgE9owDTa3wCf0IBltL8BPqEB02h/A3xCA5bR/gb4hAZMo/0N8AkNWEb7G+ATGjCN9jfAJ/x6wAtcqMOygwgKqgAAAABJRU5ErkJggg==")?;
    let image_path = workspace.join("sample.png");
    std::fs::write(&image_path, image)?;
    let mut config = profile.config.clone();
    // Only this synthetic acceptance workspace uses these native permissions.
    for (key, value) in [
        ("sandbox_mode", "read-only"),
        ("cli_auth_credentials_store", "file"),
    ] {
        config
            .as_table_mut()
            .unwrap()
            .insert(key.into(), toml::Value::String(value.into()));
    }
    if controls {
        for (key, value) in [
            ("approval_policy", "on-request"),
            ("approvals_reviewer", "user"),
        ] {
            config
                .as_table_mut()
                .unwrap()
                .insert(key.into(), toml::Value::String(value.into()));
        }
    }
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(home.join("config.toml"))?
        .write_all(toml::to_string(&config)?.as_bytes())?;
    let entry_file = directory.path().join("entry.json");
    let executable = std::env::var_os("WORKBENCH_TEST_LAUNCHER")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/codex-view"));
    let mut child = Launcher(
        tokio::process::Command::new(executable)
            .current_dir(&workspace)
            .env("HOME", directory.path())
            .env("USERPROFILE", directory.path())
            .env("CODEX_HOME", &home)
            .args(["--no-open", "--entry-file"])
            .arg(&entry_file)
            .arg("--codex-bin")
            .arg(std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?,
    );
    let entry = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(entry) = read_entry(&entry_file) {
                return Ok::<_, anyhow::Error>(entry);
            }
            ensure!(
                child.0.try_wait()?.is_none(),
                "product startup failed; private output suppressed"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;
    let mut browser = super::probe_process::ProbeProcess::spawn(
        tokio::process::Command::new("node")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r5-live-product-probe.cjs"))
            .env("WORKBENCH_PROBE_URL", &entry.url)
            .env("WORKBENCH_PROBE_IMAGE", &image_path)
            .env("WORKBENCH_PROBE_CONTROLS", if controls { "1" } else { "0" })
            .env("DEBUG", "")
            .env("PWDEBUG", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )?;
    let mut lines = BufReader::new(browser.stdout.take().unwrap()).lines();
    let outcome = tokio::time::timeout(Duration::from_secs(1000), async {
        while let Some(line) = lines.next_line().await? {
            // The browser reports only explicit, bounded evidence fields.
            let value: Value = serde_json::from_str(&line)?;
            if value["stage"] == "native-startup-diagnostic" {
                let policy = codex_local_observer::workbench::redaction::RedactionPolicy::new(profile.secrets.clone())?;
                let safe = policy.scrub(value["text"].as_str().unwrap_or(""));
                let safe = safe.as_str().replace(&profile.upstream, "[upstream]")
                    .replace(&directory.path().display().to_string(), "[temporary]");
                println!("{}", json!({"stage":"native-startup-diagnostic","sanitized":safe.chars().take(1200).collect::<String>()}));
            } else {
                println!("{value}");
            }
        }
        Ok::<_, anyhow::Error>(browser.wait().await?.success())
    })
    .await;
    browser.stdin.take(); // EOF closes Chrome even if the outer deadline fired.
    let counts = native_counts(&home);
    let config_after: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("config.toml"))?)?;
    let unchanged = profile.source_unchanged()
        && ["model", "model_provider", "model_providers"]
            .iter()
            .all(|key| config_after.get(key) == config.get(key));
    drop(child);
    let cli_gone = unsafe { libc::kill(entry.cli_pid as i32, 0) } != 0;
    let listener_gone = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?
        .get(&entry.address)
        .send()
        .await
        .is_err();
    println!(
        "{}",
        json!({"stage":"product-cleanup", "sourceAndProviderUnchanged":unchanged,
        "nativeAborts":counts.0,"nativeCompletions":counts.1,"nativeImageInputs":counts.2,
        "cliReaped":cli_gone,"listenerClosed":listener_gone,"entryRemoved":!entry_file.exists(),
        "isolatedUserHome":true})
    );
    ensure!(
        unchanged && cli_gone && listener_gone && !entry_file.exists(),
        "product cleanup/configuration failed"
    );
    ensure!(
        outcome??,
        "product browser acceptance failed; details suppressed"
    );
    ensure!(
        counts.0 >= 1 && counts.1 >= 2 && counts.2 >= 1,
        "native cancellation/completion/image evidence missing"
    );
    Ok(())
}
