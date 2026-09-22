use super::*;
use crate::workbench::config::{Config, ConfigService, Overrides, Prepared};
use crate::workbench::decode::request::{PurposeBasis, RequestInfo, RequestPurpose};
use crate::workbench::recording::{Recorder, RecorderOptions};
use portable_pty::CommandBuilder;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed Chrome; temporary JSON configuration, synthetic history and cat PTY only"]
async fn browser_settings_expand_save_conflict_and_collapse_keep_reading_and_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let cwd = dir.path().join("project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    let prepared = Prepared::load(
        &home,
        &cwd,
        &crate::workbench::paths::WorkbenchPaths::from_user_home(home.parent().unwrap()).unwrap(),
        None,
        Overrides::default(),
    )
    .unwrap();
    let root = prepared.data_dir.clone();
    let service = ConfigService::start(prepared).unwrap();
    let prior = LiveHub::new(LiveLimits::default());
    drop(
        Recorder::start(
            &prior,
            RecorderOptions::new(root.clone(), &cwd, "Synthetic previous run".into()),
        )
        .unwrap(),
    );
    let hub = LiveHub::new(LiveLimits::default());
    let _recorder = Recorder::start(
        &hub,
        RecorderOptions::new(root.clone(), &cwd, "Settings demo".into()),
    )
    .unwrap();
    let policy = RedactionPolicy::new(vec![]).unwrap();
    let request_id = Uuid::new_v4();
    hub.apply(Decoded {
        request_id,
        capture_seq: 1,
        received_at: Instant::now(),
        change: Change::Request {
            info: RequestInfo {
                client_request_index: None,
                requested_model: Some(policy.scrub("synthetic-model")),
                codex_thread_id: Some(Uuid::new_v4()),
                codex_turn_id: Some("settings-demo".into()),
                purpose: RequestPurpose::Conversation,
                purpose_basis: PurposeBasis::CodexTurnMetadata,
            },
        },
    });
    for index in 0..20 {
        hub.apply(Decoded {
            request_id,
            capture_seq: index + 2,
            received_at: Instant::now(),
            change: Change::TextDelta {
                key: TextKey {
                    request_id,
                    response_id: Some("synthetic-response".into()),
                    wire_item_id: format!("msg-{index}"),
                    content_index: 0,
                },
                text: policy.scrub(&format!(
                    "配置面板体验验证 {index}\n\n在当前工作台展开设置，保留对话与右侧原生终端。"
                )),
            },
        });
    }
    let mut command = CommandBuilder::new("/bin/cat");
    command.cwd(&cwd);
    command.env("CODEX_HOME", &home);
    let host =
        crate::workbench::terminal::TerminalHost::spawn(hub.epoch(), command, 24, 80).unwrap();
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let server = ReadingServer::bind_configured(
        hub.clone(),
        host.handle(),
        listener,
        Some(service.handle()),
        None,
    )
    .await
    .unwrap();
    let mut command = tokio::process::Command::new("node");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/web/e2e/r31-settings-probe.cjs"
        ))
        .env("WORKBENCH_PROBE_URL", server.bootstrap_url())
        .env("DEBUG", "")
        .env("PWDEBUG", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("WORKBENCH_TEST_SCREENSHOT") {
        command.env("WORKBENCH_PROBE_SCREENSHOT", path);
    }
    let mut browser = crate::workbench::probe_process::ProbeProcess::spawn(&mut command).unwrap();
    let mut liveness = browser.stdin.take().unwrap();
    let mut lines = BufReader::new(browser.stdout.take().unwrap()).lines();
    let mut complete = false;
    tokio::time::timeout(Duration::from_secs(55), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let report: Value = serde_json::from_str(&line).unwrap();
            println!("{report}");
            match report["stage"].as_str().unwrap() {
                "saved" => {
                    let bytes = std::fs::read(home.parent().unwrap().join(".codex-web/config/config.json")).unwrap();
                    assert_eq!(
                        Config::parse(&bytes).unwrap().launch.profile.as_deref(),
                        Some("future_profile")
                    );
                }
                "external" => {
                    let mut config = Config::default();
                    config.launch.profile = Some("external_profile".into());
                    std::fs::write(
                        home.parent().unwrap().join(".codex-web/config/config.json"),
                        serde_json::to_vec(&config).unwrap(),
                    )
                    .unwrap();
                }
                "complete" => complete = true,
                "retention-fixture" => {
                    let old=LiveHub::new(LiveLimits::default());
                    drop(Recorder::start(&old,RecorderOptions::new(root.clone(),&cwd,"Synthetic expired run".into())).unwrap());
                    let run=root.join("runs").join(old.epoch().to_string());
                    let mut meta:Value=serde_json::from_slice(&std::fs::read(run.join("meta.json")).unwrap()).unwrap();
                    meta["startedAt"]=json!((chrono::Utc::now()-chrono::Duration::days(3)).to_rfc3339());
                    let bytes=serde_json::to_vec(&meta).unwrap();std::fs::write(run.join("meta.json"),&bytes).unwrap();
                    let proof=json!({"schemaVersion":1,"runEpoch":old.epoch(),"workspaceId":meta["workspaceId"],"endedAt":(chrono::Utc::now()-chrono::Duration::days(2)).to_rfc3339(),"finalMetaDigest":blake3::hash(&bytes).to_hex().to_string(),"savedRecordSeq":meta["savedRecordSeq"]});
                    std::fs::write(run.join("lifecycle.json"),serde_json::to_vec(&proof).unwrap()).unwrap();
                    liveness.write_all(b"retention-ready\n").await.unwrap();
                }
                _ => panic!("settings browser probe failed at safe stage"),
            }
        }
        assert!(browser.wait().await.unwrap().success());
    })
    .await
    .unwrap();
    assert!(complete);
}
