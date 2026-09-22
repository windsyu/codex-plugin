use super::*;
use crate::workbench::decode::{ResponseStatus, details::usage};
use crate::workbench::recording::{Recorder, RecorderOptions};
use portable_pty::CommandBuilder;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed Chrome; synthetic model usage and cat PTY only"]
async fn browser_usage_overview_updates_without_recounting_or_changing_terminal_input() {
    let dir = tempfile::tempdir().unwrap();
    let hub = LiveHub::new(LiveLimits::default());
    let _recorder = Recorder::start(
        &hub,
        RecorderOptions::new(
            dir.path().join("history"),
            dir.path(),
            "Token usage demo".into(),
        ),
    )
    .unwrap();
    let mut command = CommandBuilder::new("/bin/cat");
    command.cwd(dir.path());
    command.env("CODEX_HOME", dir.path());
    let host =
        crate::workbench::terminal::TerminalHost::spawn(hub.epoch(), command, 24, 80).unwrap();
    let server = ReadingServer::bind_with_terminal(hub.clone(), host.handle())
        .await
        .unwrap();
    let request = Uuid::new_v4();
    let emit = |name: &str, status, counts| {
        hub.apply(Decoded {
            request_id: request,
            capture_seq: 1,
            received_at: Instant::now(),
            change: Change::Response {
                response_id: Some(name.into()),
                status,
                model: Some(
                    RedactionPolicy::new(vec![])
                        .unwrap()
                        .scrub("synthetic-model"),
                ),
                usage: usage(&counts),
            },
        })
    };
    let first = json!({"input_tokens":10000,"output_tokens":2000,"input_tokens_details":{"cached_tokens":8000},"output_tokens_details":{"reasoning_tokens":500}});
    let second = json!({"input_tokens":6000,"output_tokens":3000,"input_tokens_details":{"cached_tokens":1000},"output_tokens_details":{"reasoning_tokens":1000}});
    let mut command = tokio::process::Command::new("node");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/web/e2e/r3-usage-overview-probe.cjs"
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
    tokio::time::timeout(Duration::from_secs(45), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let report: Value = serde_json::from_str(&line).unwrap();
            println!("{report}");
            match report["stage"].as_str().unwrap() {
                "partial" => {
                    emit("one", ResponseStatus::Completed, first.clone());
                    emit("two", ResponseStatus::Receiving, Value::Null);
                }
                "reported" => {
                    emit("two", ResponseStatus::Completed, second.clone());
                    emit("one", ResponseStatus::Completed, first.clone());
                }
                "complete" => complete = true,
                _ => panic!("usage browser assertion failed at safe stage"),
            }
        }
        assert!(browser.wait().await.unwrap().success());
    })
    .await
    .unwrap();
    assert!(complete);
    assert_eq!(
        hub.snapshot().usage_summary.total_tokens.tokens,
        Some(21000)
    );
}
