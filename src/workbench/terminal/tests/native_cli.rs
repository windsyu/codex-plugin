//! Opt-in ordinary installed CLI; isolated home and no submitted model prompt.
use super::*;

#[tokio::test]
#[ignore = "requires installed official Codex CLI and PTY; isolated initialization only"]
async fn installed_cli_initialization_draft_refresh_and_native_exit_use_the_terminal_actor() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("codex-home");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("README.md"), "# Isolated PTY test\n").unwrap();
    std::fs::write(
        home.join("config.toml"),
        r#"model = "gpt-6-astra"
model_provider = "custom"
check_for_update_on_startup = false
[model_providers.custom]
name = "Synthetic terminal test"
base_url = "http://127.0.0.1:1/unreachable/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "synthetic-terminal-only"
"#,
    )
    .unwrap();
    let mut command = CommandBuilder::new(
        std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into()),
    );
    command.cwd(&workspace);
    command.env("HOME", &home);
    command.env("USERPROFILE", &home);
    command.env("CODEX_HOME", &home);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    let host = TerminalHost::spawn(Uuid::new_v4(), command, 45, 120).unwrap();
    let handle = host.handle();
    let first = handle.attach().await.unwrap();
    let grant = handle.claim(first.connection_id).await.unwrap();
    let mut seq = 0;
    let mut themed = false;
    let mut trusted = false;
    let mut ready_at = None;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let snapshot = handle.attach().await.unwrap();
            let contents = screen(&snapshot.snapshot);
            if (!themed
                && (contents.contains("Choose your style") || contents.contains("Select a theme")))
                || (!trusted
                    && (contents.contains("Do you trust")
                        || contents.contains("Do you want to work in this directory")
                        || contents.contains("Trust this folder?")))
            {
                if contents.contains("Choose your style") || contents.contains("Select a theme") {
                    themed = true;
                } else {
                    trusted = true;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
                seq += 1;
                handle
                    .input(first.connection_id, grant.generation, seq, b"\r".to_vec())
                    .await
                    .unwrap();
            } else if contents.contains("OpenAI Codex")
                && contents.contains('›')
                && ready_at.get_or_insert_with(Instant::now).elapsed() > Duration::from_millis(300)
            {
                break;
            }
            drop(snapshot);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native initialization deadline; raw screen suppressed");
    seq += 1;
    handle
        .input(
            first.connection_id,
            grant.generation,
            seq,
            "\x1b[200~中文未提交草稿\x1b[201~".as_bytes().to_vec(),
        )
        .await
        .unwrap();
    let refreshed = wait_screen(&handle, "中文未提交草稿").await;
    assert_eq!(
        refreshed.control.controller_connection,
        Some(first.connection_id)
    );
    let fresh_grant = handle
        .reconnect(refreshed.connection_id, grant.reconnect_secret)
        .await
        .unwrap();
    drop(first);
    handle
        .input(
            refreshed.connection_id,
            fresh_grant.generation,
            1,
            b"\x15".to_vec(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    handle
        .input(
            refreshed.connection_id,
            fresh_grant.generation,
            2,
            b"\x04".to_vec(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ended = handle.attach().await.unwrap();
            if let Some(exit) = ended.exit {
                assert_eq!(exit.code, 0);
                assert!(ended.control.ended);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native Ctrl-D exit deadline");
    // Draft was never submitted, so no user task should have a rollout.
    assert!(!home.join("sessions").exists());
}
