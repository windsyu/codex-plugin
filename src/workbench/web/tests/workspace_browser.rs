use super::*;
use crate::workbench::{terminal::TerminalHost, workspace::Handle};
use portable_pty::CommandBuilder;
use std::process::{Command, Stdio};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed Chrome; synthetic Git project and cat PTY"]
async fn browser_workspace_files_search_git_and_responsive_navigation_keep_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let home = dir.path().join("home");
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&home).unwrap();
    for path in ["src/components", "docs", "config"] {
        std::fs::create_dir_all(project.join(path)).unwrap();
    }
    for (path, content) in [
        (
            "src/components/MessageList.tsx",
            "export const spacing = 8;\n",
        ),
        (
            "src/components/对话内容与工具执行结果展示.tsx",
            "export const groupTools = false;\n",
        ),
        ("src/legacy.ts", "export const legacy = true;\n"),
        ("docs/hello.ts", "export const example = 'before';\n"),
        (
            "docs/notes.md",
            "# Synthetic notes\n\nRead files, search the project, and inspect Git changes.\n",
        ),
        ("config/layout.json", "{\"theme\":\"dark\"}\n"),
        (
            "package.json",
            "{\"name\":\"workbench-demo\",\"private\":true}\n",
        ),
    ] {
        std::fs::write(project.join(path), content).unwrap();
    }
    std::fs::write(
        project.join("README.md"),
        "# Synthetic workspace\n\nFiles, search and Git reading.\n",
    )
    .unwrap();
    std::fs::write(
        project.join("src/hello.ts"),
        "export const greeting = '你好，工作台';\nexport const mode = 'before';\n",
    )
    .unwrap();
    for args in [
        vec!["init", "-q", "-b", "codex/workbench-ui"],
        vec!["add", "."],
        vec!["commit", "-qm", "Add synthetic workspace"],
    ] {
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=Synthetic",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "commit.gpgsign=false"
                ])
                .args(args)
                .current_dir(&project)
                .env("HOME", &home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    std::fs::write(
        project.join("src/hello.ts"),
        "export const greeting = '你好，工作台';\nexport const mode = 'staged';\n",
    )
    .unwrap();
    std::fs::rename(
        project.join("docs/notes.md"),
        project.join("docs/getting-started.md"),
    )
    .unwrap();
    assert!(
        Command::new("git")
            .args(["add", "."])
            .current_dir(&project)
            .env("HOME", &home)
            .output()
            .unwrap()
            .status
            .success()
    );
    std::fs::write(project.join("src/hello.ts"),"export const greeting = '你好，工作台';\nexport const mode = 'working';\n// <img onerror=alert(1)> literal\n").unwrap();
    std::fs::remove_file(project.join("src/legacy.ts")).unwrap();
    std::fs::write(
        project.join("docs/hello.ts"),
        "export const example = 'after';\n",
    )
    .unwrap();
    std::fs::write(
        project.join("src/components/对话内容与工具执行结果展示.tsx"),
        "export const groupTools = true;\n",
    )
    .unwrap();
    std::fs::write(
        project.join("-新文件.txt"),
        "needle on line one\nneedle on line two\n",
    )
    .unwrap();
    std::fs::write(project.join(".env"), "SYNTHETIC_SECRET=never-visible\n").unwrap();
    for (name, content) in [
        (
            "continuous.txt",
            (1..=302)
                .map(|n| format!("ROW_{n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        (
            "large.txt",
            (1..=30_000)
                .map(|n| format!("ROW_{n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        (
            "long-line.txt",
            format!("{}LONG_LINE_END", "x".repeat(20_000)),
        ),
        ("dense.txt", format!("{}x", "x\n".repeat(499_999))),
        ("oversize.txt", "x".repeat(1024 * 1024 + 1)),
        ("empty.txt", String::new()),
    ] {
        std::fs::write(project.join("docs").join(name), content).unwrap();
    }
    let hub = LiveHub::new(LiveLimits::default());
    let request_id = Uuid::new_v4();
    let policy = RedactionPolicy::new(vec![]).unwrap();
    hub.apply(Decoded {
        request_id,
        capture_seq: 1,
        received_at: Instant::now(),
        change: Change::Request {
            info: crate::workbench::decode::request::RequestInfo {
                client_request_index: None,
                requested_model: Some(policy.scrub("synthetic-model")),
                codex_thread_id: Some(Uuid::new_v4()),
                codex_turn_id: Some("workspace-demo".into()),
                purpose: crate::workbench::decode::request::RequestPurpose::Conversation,
                purpose_basis: crate::workbench::decode::request::PurposeBasis::CodexTurnMetadata,
            },
        },
    });
    hub.apply(Decoded { request_id,capture_seq:2,received_at:Instant::now(),change:Change::TextDelta { key:TextKey {request_id,response_id:Some("synthetic-response".into()),wire_item_id:"msg".into(),content_index:0},text:policy.scrub("已完成项目结构检查。可以从左侧阅读文件、搜索代码或查看 Git 变更；右侧终端继续接收输入。") } });
    let mut command = CommandBuilder::new("/bin/cat");
    command.cwd(&project);
    command.env("HOME", &home);
    command.env("CODEX_HOME", &home);
    let host = TerminalHost::spawn(hub.epoch(), command, 24, 80).unwrap();
    let server = ReadingServer::bind_configured(
        hub,
        host.handle(),
        std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        None,
        Some(Handle::start(&project).unwrap()),
    )
    .await
    .unwrap();
    let mut command = tokio::process::Command::new("node");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/web/e2e/r4-workspace-probe.cjs"
        ))
        .env("WORKBENCH_PROBE_URL", server.bootstrap_url())
        .env("WORKBENCH_PROBE_PROJECT", &project)
        .env("DEBUG", "")
        .env("PWDEBUG", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("WORKBENCH_TEST_SCREENSHOT") {
        command.env("WORKBENCH_PROBE_SCREENSHOT", path);
    }
    let mut browser = crate::workbench::probe_process::ProbeProcess::spawn(&mut command).unwrap();
    let _liveness = browser.stdin.take().unwrap();
    let mut lines = BufReader::new(browser.stdout.take().unwrap()).lines();
    let mut complete = false;
    tokio::time::timeout(Duration::from_secs(55), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let value: Value = serde_json::from_str(&line).unwrap();
            println!("{value}");
            if value["stage"] == "complete" {
                complete = true;
            } else {
                assert_eq!(value["stage"], "progress");
            }
        }
        assert!(browser.wait().await.unwrap().success());
    })
    .await
    .unwrap();
    assert!(complete);
}
