//! The actual product binary, its original home/profile, and a real browser.
use super::*;
use crate::workbench::launch::{BrowserEntry, read_entry};
use std::path::Path;
use std::process::Stdio;

fn reply(text: String, index: usize, pace: bool) -> Response {
    let stream = async_stream::stream! {
        let response=format!("resp_launcher_{index}");let item=format!("msg_launcher_{index}");
        yield Ok::<_,std::io::Error>(event(json!({"type":"response.created","response":{"id":response,"model":"gpt-6-astra"}})));
        yield Ok(event(json!({"type":"response.output_item.added","item":{"id":item,"type":"message","role":"assistant","content":[]}})));
        for chunk in text.chars().collect::<Vec<_>>().chunks(12) {
            if pace {tokio::time::sleep(Duration::from_millis(45)).await;}
            yield Ok(event(json!({"type":"response.output_text.delta","item_id":item,"content_index":0,"delta":chunk.iter().collect::<String>()})));
        }
        yield Ok(event(json!({"type":"response.output_item.done","item":{"id":item,"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}})));
        yield Ok(event(json!({"type":"response.completed","response":{"id":response}})));
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}
fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires freshly built codex-view and installed CLI; no model requests"]
async fn closing_launch_terminal_cleans_native_cli_listener_and_entry() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("native-home");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(
        home.join("config.toml"),
        r#"model="synthetic-model"
model_provider="custom"
check_for_update_on_startup=false
[model_providers.custom]
name="Synthetic shutdown fixture"
base_url="http://127.0.0.1:1/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-shutdown-token"
"#,
    )
    .unwrap();
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let entry_file = directory.path().join(format!("entry-{signal}.json"));
        let mut child = LauncherChild(
            launcher_command(&home, &workspace, &entry_file)
                .spawn()
                .unwrap(),
        );
        let entry = wait_entry(&mut child, &entry_file).await;
        assert!(alive(entry.cli_pid));
        assert_eq!(
            unsafe { libc::kill(child.0.id().unwrap() as i32, signal) },
            0
        );
        let status = timeout(Duration::from_secs(5), child.0.wait())
            .await
            .unwrap()
            .unwrap();
        let native_gone = timeout(Duration::from_secs(3), async {
            while alive(entry.cli_pid) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok();
        // Even a failing pre-fix run must not leave its fixture CLI behind.
        if !native_gone {
            unsafe { libc::kill(entry.cli_pid as i32, libc::SIGKILL) };
        }
        assert!(
            status.success(),
            "signal {signal} bypassed graceful shutdown"
        );
        assert!(native_gone, "signal {signal} left the owned CLI alive");
        assert!(!entry_file.exists(), "signal {signal} left a pairing entry");
        let client = Client::builder().no_proxy().build().unwrap();
        assert!(client.get(&entry.address).send().await.is_err());
    }
}

// An assertion failure must still give the launcher time to stop its PTY.
pub(super) struct LauncherChild(pub(super) tokio::process::Child);
impl Drop for LauncherChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        if let Some(pid) = self.0.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            let until = Instant::now() + Duration::from_secs(5);
            while Instant::now() < until {
                if self.0.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = self.0.start_kill();
        }
    }
}

pub(super) fn launcher_command(
    home: &Path,
    workspace: &Path,
    entry: &Path,
) -> tokio::process::Command {
    let executable = std::env::var_os("WORKBENCH_TEST_LAUNCHER")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/codex-view"));
    let mut command = tokio::process::Command::new(executable);
    command
        .current_dir(workspace)
        // Only this fixture child sees a temporary user home. CODEX_HOME stays
        // separate so default workbench paths cannot touch real user files.
        .env("HOME", home.parent().expect("temporary fixture home"))
        .env(
            "USERPROFILE",
            home.parent().expect("temporary fixture home"),
        )
        .env("CODEX_HOME", home)
        .args(["--no-open", "--entry-file"])
        .arg(entry)
        .arg("--codex-bin")
        .arg(std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

pub(super) async fn wait_entry(child: &mut LauncherChild, path: &Path) -> BrowserEntry {
    timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(entry) = read_entry(path) {
                break entry;
            }
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "launcher exited before readiness (private output suppressed)"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires freshly built codex-view, installed CLI and Chrome; synthetic upstream only"]
async fn product_launcher_preserves_native_profile_and_browser_flow_and_cleans_owned_run() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("launcher-project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir_all(workspace.join(".codex")).unwrap();
    std::fs::write(workspace.join("R4-check.txt"), "native workspace probe\n").unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let upstream=fixture(move|request|{
        let seen=seen.clone();
        async move {
            assert_eq!(request.headers().get(header::AUTHORIZATION).unwrap(),"Bearer synthetic-launcher-profile");
            let body:Value=serde_json::from_slice(&axum::body::to_bytes(request.into_body(),2*1024*1024).await.unwrap()).unwrap();
            if body.pointer("/text/format/schema/properties/title").is_some(){ return reply("{\"title\":\"本机工作台调试\"}".into(),0,false); }
            assert_eq!(body["model"],"gpt-6-astra");
            assert_eq!(body["reasoning"]["effort"],"low");
            let index=seen.fetch_add(1,Ordering::SeqCst)+1;
            let text=format!("R1_NATIVE_BROWSER_OK\n\n正式 Launcher 的合成流式回复。\n\n{}\n\nR1_STREAM_DONE",(1..=14).map(|i|format!("{i}. 用户通过原生终端提交中文多行，中央按来源区分用户和模型；切换、刷新及接管均保持同一 CLI，草稿不自动发送。\n\n")).collect::<String>());
            reply(text,index,true)
        }
    }).await;
    let base = r#"model="gpt-6-astra"
model_provider="custom"
check_for_update_on_startup=false
cli_auth_credentials_store="file"
[model_providers.custom]
name="Synthetic launcher provider"
base_url="http://127.0.0.1:1/wrong-base/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-launcher-base"
"#;
    let profile = format!(
        "[model_providers.custom]\nname=\"Synthetic profile provider\"\nbase_url=\"http://{}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nexperimental_bearer_token=\"synthetic-launcher-profile\"\n",
        upstream.address
    );
    let project = r#"model_reasoning_effort="low"
[model_providers.custom]
name="Synthetic project provider"
base_url="http://127.0.0.1:1/wrong-project/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-launcher-project"
"#;
    std::fs::write(home.join("config.toml"), base).unwrap();
    std::fs::write(home.join("named.config.toml"), &profile).unwrap();
    std::fs::write(workspace.join(".codex/config.toml"), project).unwrap();
    use crate::workbench::config::{Config, Overrides, Prepared};
    let preferences = directory.path().join("preferences");
    drop(
        Prepared::load(
            &home,
            &workspace,
            &crate::workbench::paths::WorkbenchPaths::from_user_home(home.parent().unwrap())
                .unwrap(),
            Some(&preferences),
            Overrides::default(),
        )
        .unwrap(),
    );
    let mut saved = Config::default();
    saved.launch.profile = Some("unselected_saved_profile".into());
    saved.storage.data_dir = Some(directory.path().join("history-saved"));
    let saved_bytes = serde_json::to_vec(&saved).unwrap();
    std::fs::write(preferences.join("config.json"), &saved_bytes).unwrap();
    let entry_file = directory.path().join("entry.json");
    let mut child = LauncherChild(
        launcher_command(&home, &workspace, &entry_file)
            .args([
                "--profile",
                "named",
                "--config-dir",
                "../preferences",
                "--data-dir",
                "../history-override",
            ])
            .spawn()
            .unwrap(),
    );
    let launcher_pid = child.0.id().unwrap();
    let entry = wait_entry(&mut child, &entry_file).await;
    assert_eq!(
        count.load(Ordering::SeqCst),
        0,
        "launching must not submit a task"
    );
    let mut browser = tokio::process::Command::new("node");
    browser
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/e2e/r1-terminal-probe.cjs"))
        .env("WORKBENCH_PROBE_URL", &entry.url)
        .env("WORKBENCH_PROBE_WORKSPACE", "true")
        .env("WORKBENCH_PROBE_LATE_USER", "false")
        .env("WORKBENCH_PROBE_UNKNOWN", "false")
        .env("DEBUG", "")
        .env("PWDEBUG", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("WORKBENCH_TEST_SCREENSHOT") {
        browser.env("WORKBENCH_PROBE_SCREENSHOT", path);
    }
    let mut browser = crate::workbench::probe_process::ProbeProcess::spawn(&mut browser).unwrap();
    let _liveness = browser.stdin.take();
    let status = timeout(Duration::from_secs(100), browser.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        status.success(),
        "product launcher browser acceptance failed"
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(!alive(entry.cli_pid), "native exit should reap the CLI");
    assert!(
        alive(launcher_pid),
        "reading service remains available after native CLI exit"
    );
    let client = Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        std::fs::read(preferences.join("config.json")).unwrap(),
        saved_bytes
    );
    assert!(!directory.path().join("history-saved").exists());
    assert!(
        directory
            .path()
            .join("history-override/runs")
            .join(entry.run_epoch.to_string())
            .join("meta.json")
            .exists()
    );
    assert!(
        !home.parent().unwrap().join(".codex-web/config").exists(),
        "custom config directory is the only configuration source"
    );
    assert_eq!(
        client.get(&entry.address).send().await.unwrap().status(),
        StatusCode::OK
    );
    let current: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
    let original: toml::Value = toml::from_str(base).unwrap();
    assert_eq!(current["model_providers"], original["model_providers"]);
    assert_eq!(current["model"], original["model"]);
    let mut saved_profile: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("named.config.toml")).unwrap()).unwrap();
    // Native trust is written by the CLI into the selected named profile.
    assert!(saved_profile["projects"].as_table().is_some());
    saved_profile.as_table_mut().unwrap().remove("projects");
    assert_eq!(
        saved_profile,
        toml::from_str::<toml::Value>(&profile).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join(".codex/config.toml")).unwrap(),
        project
    );
    assert_eq!(unsafe { libc::kill(launcher_pid as i32, libc::SIGTERM) }, 0);
    assert!(
        timeout(Duration::from_secs(5), child.0.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!entry_file.exists());
    assert!(
        client.get(&entry.address).send().await.is_err(),
        "launcher exit must close the reading listener"
    );
    println!(
        "{}",
        json!({"check":"product-launcher","mainRequests":2,"namedProfilePreserved":true,"projectRoutingIgnored":true,"nativeExitReadable":true,"sigtermCleaned":true,"relativeConfigDirectory":true,"explicitProfileAndStorageOverrides":true,"savedJsonUnchanged":true})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires freshly built codex-view and installed CLI; temporary home and loopback only"]
async fn product_launcher_stop_and_signals_end_only_the_owned_native_process() {
    let mut unrelated = tokio::process::Command::new("/bin/sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for web_stop in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(
            home.join("config.toml"),
            r#"model="gpt-6-astra"
model_provider="custom"
check_for_update_on_startup=false
[model_providers.custom]
name="Synthetic lifecycle provider"
base_url="http://127.0.0.1:1/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-lifecycle-token"
"#,
        )
        .unwrap();
        let entry_file = directory.path().join("entry.json");
        let mut child = LauncherChild(
            launcher_command(&home, &workspace, &entry_file)
                .spawn()
                .unwrap(),
        );
        let entry = wait_entry(&mut child, &entry_file).await;
        let root = directory.path().canonicalize().unwrap().join(".codex-web");
        assert!(root.join("config/config.json").is_file());
        timeout(Duration::from_secs(5), async {
            while !root
                .join("history/runs")
                .join(entry.run_epoch.to_string())
                .join("meta.json")
                .is_file()
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(!home.join("workbench").exists());
        assert!(!home.join("workbench-data-v1").exists());
        assert!(alive(entry.cli_pid));
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        if web_stop {
            let token = reqwest::Url::parse(&entry.url)
                .unwrap()
                .fragment()
                .unwrap()
                .strip_prefix("pair=")
                .unwrap()
                .to_owned();
            let response = client
                .post(format!("{}/workbench/v1/pair", entry.address))
                .header(header::ORIGIN, &entry.address)
                .header(header::CONTENT_TYPE, "application/json")
                .body(json!({"token":token}).to_string())
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            let cookie = response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned();
            for _ in 0..2 {
                let response = client
                    .post(format!("{}/workbench/v1/run/stop", entry.address))
                    .header(header::ORIGIN, &entry.address)
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(json!({"epoch":entry.run_epoch}).to_string())
                    .send()
                    .await
                    .unwrap();
                assert!(response.status().is_success());
            }
            timeout(Duration::from_secs(5), async {
                while alive(entry.cli_pid) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            use tokio_tungstenite::tungstenite::client::IntoClientRequest;
            let mut request = format!(
                "{}/workbench/v1/terminal?epoch={}",
                entry.address.replacen("http://", "ws://", 1),
                entry.run_epoch
            )
            .into_client_request()
            .unwrap();
            request
                .headers_mut()
                .insert(header::ORIGIN, entry.address.parse().unwrap());
            request
                .headers_mut()
                .insert(header::COOKIE, cookie.parse().unwrap());
            let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
            let frame = timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let run: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_eq!(run["type"], "snapshot");
            assert!(
                !run["exit"].is_null(),
                "stopped native CLI should remain readable as ended"
            );
            assert!(child.0.try_wait().unwrap().is_none());
        }
        let signal = if web_stop {
            libc::SIGTERM
        } else {
            libc::SIGINT
        };
        assert_eq!(
            unsafe { libc::kill(child.0.id().unwrap() as i32, signal) },
            0
        );
        assert!(
            timeout(Duration::from_secs(5), child.0.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        assert!(!alive(entry.cli_pid));
        assert!(unrelated.try_wait().unwrap().is_none());
        assert!(!entry_file.exists());
        assert!(client.get(&entry.address).send().await.is_err());
        if web_stop {
            // Roll back only after the new Run and its listeners have ended.
            // Start plain official CLI with the same isolated native config;
            // no proxy override, no model task, and no second controller.
            let mut command = portable_pty::CommandBuilder::new(
                std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into()),
            );
            command.cwd(&workspace);
            command.env("HOME", directory.path());
            command.env("USERPROFILE", directory.path());
            command.env("CODEX_HOME", &home);
            command.env("TERM", "xterm-256color");
            let fallback =
                crate::workbench::terminal::TerminalHost::spawn(Uuid::new_v4(), command, 45, 120)
                    .unwrap();
            let fallback_pid = fallback.process_id();
            assert!(!alive(entry.cli_pid));
            assert_ne!(fallback_pid, entry.cli_pid);
            let handle = fallback.handle();
            timeout(Duration::from_secs(10), async {
                loop {
                    let attached = handle.attach().await.unwrap();
                    let mut screen =
                        vt100::Parser::new(attached.snapshot.rows, attached.snapshot.cols, 0);
                    screen.process(&attached.snapshot.screen);
                    screen.process(&attached.snapshot.replay);
                    let text = screen.screen().contents();
                    drop(attached);
                    if text.contains("OpenAI Codex")
                        || text.contains("Choose your style")
                        || text.contains("Select a theme")
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            assert!(alive(fallback_pid));
            drop(fallback);
            timeout(Duration::from_secs(5), async {
                while alive(fallback_pid) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
        }
    }
    unrelated.kill().await.unwrap();
    unrelated.wait().await.unwrap();
    println!(
        "{}",
        json!({"check":"product-launcher-lifecycle","activeSigintCleaned":true,"webStopIdempotent":true,"stoppedRunReadable":true,"unrelatedProcessPreserved":true,"defaultProductDirectories":true,"nativeHomeSeparated":true,"plainCliFallbackAfterRunEnded":true})
    );
}
