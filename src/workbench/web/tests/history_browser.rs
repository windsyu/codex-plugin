use super::*;
use crate::workbench::decode::request::{PurposeBasis, RequestInfo, RequestPurpose};
use crate::workbench::recording::{Recorder, RecorderOptions};
use portable_pty::CommandBuilder;
use std::process::Stdio;
use std::sync::atomic::Ordering;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed Chrome; synthetic history faults and a cat PTY only"]
async fn browser_reports_save_failure_and_recovery_while_terminal_and_reading_continue() {
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let mut options = RecorderOptions::new(
        dir.path().join("history"),
        dir.path(),
        "R3 synthetic storage fault".into(),
    );
    options.sync_interval = Duration::from_millis(50);
    let faults = options.faults.clone();
    let _recorder = Recorder::start(&hub, options).unwrap();
    let mut command = CommandBuilder::new("/bin/cat");
    command.cwd(dir.path());
    command.env("CODEX_HOME", dir.path());
    let host =
        crate::workbench::terminal::TerminalHost::spawn(hub.epoch(), command, 24, 80).unwrap();
    let server = ReadingServer::bind_with_terminal(hub.clone(), host.handle())
        .await
        .unwrap();
    let request_id = Uuid::new_v4();
    let thread = Uuid::new_v4();
    hub.apply(Decoded {
        request_id,
        capture_seq: 1,
        received_at: Instant::now(),
        change: Change::Request {
            info: RequestInfo {
                client_request_index: None,
                requested_model: Some(RedactionPolicy::new(vec![]).unwrap().scrub("synthetic")),
                codex_thread_id: Some(thread),
                codex_turn_id: Some("r3-fixture".into()),
                purpose: RequestPurpose::Conversation,
                purpose_basis: PurposeBasis::CodexTurnMetadata,
            },
        },
    });
    let key = TextKey {
        request_id,
        response_id: Some("r3-fixture-response".into()),
        wire_item_id: "r3-fixture-message".into(),
        content_index: 0,
    };
    let emit = |text: &str| {
        hub.apply(Decoded {
            request_id,
            capture_seq: 2,
            received_at: Instant::now(),
            change: Change::TextDelta {
                key: key.clone(),
                text: RedactionPolicy::new(vec![]).unwrap().scrub(text),
            },
        })
    };
    emit("R3_ALREADY_SAVED：保存故障的合成演示。\n\n");
    let mut command = tokio::process::Command::new("node");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/web/e2e/r3-storage-fault-probe.cjs"
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
    let _liveness = browser.stdin.take();
    let mut lines = BufReader::new(browser.stdout.take().unwrap()).lines();
    let mut complete = false;
    tokio::time::timeout(Duration::from_secs(40), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let report: Value = serde_json::from_str(&line).unwrap();
            println!("{report}");
            match report["stage"].as_str().unwrap() {
                "degrade" => {
                    faults.write_error.store(true, Ordering::Release);
                    emit("R3_MEMORY_CONTINUES：记录失败时文字继续显示。\n\n");
                }
                "recover" => {
                    faults.write_error.store(false, Ordering::Release);
                    emit("R3_RECORDING_RESUMES：从新分段恢复。\n\n");
                }
                "complete" => complete = true,
                _ => panic!("storage fault browser assertion failed at safe stage"),
            }
        }
        assert!(browser.wait().await.unwrap().success());
    })
    .await
    .unwrap();
    assert!(complete);
}
