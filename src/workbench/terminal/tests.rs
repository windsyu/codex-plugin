use super::*;
use std::path::Path;

mod native_cli;

fn host(directory: &Path, script: &str) -> TerminalHost {
    // This executable is a deterministic PTY fixture, never a product fallback.
    let mut command = CommandBuilder::new("/bin/sh");
    command.args(["-c", script]);
    command.cwd(directory);
    command.env("CODEX_HOME", directory);
    command.env("TERM", "xterm-256color");
    TerminalHost::spawn(Uuid::new_v4(), command, 24, 80).unwrap()
}

const ECHO: &str = "stty -echo; printf 'READY\\r\\n'; while IFS= read -r line; do printf '%s\\n' \"$line\" >> inputs.txt; printf 'REPLY:%s\\r\\n' \"$line\"; done";

fn screen(snapshot: &TerminalSnapshot) -> String {
    let mut parser = vt100::Parser::new(snapshot.rows, snapshot.cols, 0);
    parser.process(&snapshot.screen);
    parser.process(&snapshot.replay);
    parser.screen().contents()
}

async fn wait_screen(handle: &TerminalHandle, expected: &str) -> Attachment {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let attachment = handle.attach().await.unwrap();
            if screen(&attachment.snapshot).contains(expected) {
                return attachment;
            }
            drop(attachment);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("PTY did not display synthetic marker {expected}"))
}

#[tokio::test]
async fn native_bytes_roles_takeover_and_duplicate_input_share_one_serial_owner() {
    let directory = tempfile::tempdir().unwrap();
    let host = host(directory.path(), ECHO);
    let handle = host.handle();
    let first = wait_screen(&handle, "READY").await;
    let second = handle.attach().await.unwrap();
    assert!(
        handle
            .input(second.connection_id, 0, 1, b"denied\n".to_vec())
            .await
            .is_err()
    );
    let grant = handle.claim(first.connection_id).await.unwrap();
    handle
        .input(
            first.connection_id,
            grant.generation,
            1,
            "中文第一行\n第二行\n".as_bytes().to_vec(),
        )
        .await
        .unwrap();
    wait_screen(&handle, "REPLY:第二行").await;
    assert_eq!(
        handle
            .input(
                first.connection_id,
                grant.generation,
                1,
                b"duplicate\n".to_vec()
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Control(ControlError::InputSequence)
    );
    let next = handle.takeover(second.connection_id).await.unwrap();
    assert_eq!(
        handle
            .input(
                first.connection_id,
                grant.generation,
                2,
                b"stale\n".to_vec()
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Control(ControlError::StaleGeneration)
    );
    handle
        .input(
            second.connection_id,
            next.generation,
            1,
            b"new owner\n".to_vec(),
        )
        .await
        .unwrap();
    wait_screen(&handle, "REPLY:new owner").await;
    assert_eq!(
        std::fs::read_to_string(directory.path().join("inputs.txt")).unwrap(),
        "中文第一行\n第二行\nnew owner\n"
    );
}

#[tokio::test]
async fn snapshot_plus_registered_tail_is_contiguous_and_refresh_does_not_spawn() {
    let directory = tempfile::tempdir().unwrap();
    let host = host(directory.path(), ECHO);
    let pid = host.process_id();
    let handle = host.handle();
    let first = wait_screen(&handle, "READY").await;
    let grant = handle.claim(first.connection_id).await.unwrap();
    let mut refreshed = handle.attach().await.unwrap();
    let mut seq = refreshed.snapshot.to_seq;
    let mut parser = vt100::Parser::new(24, 80, 0);
    parser.process(&refreshed.snapshot.screen);
    parser.process(&refreshed.snapshot.replay);
    handle
        .input(
            first.connection_id,
            grant.generation,
            1,
            b"after snapshot\n".to_vec(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !parser.screen().contents().contains("REPLY:after snapshot") {
            if let TerminalEvent::Output(output) = refreshed.events.recv().await.unwrap() {
                assert_eq!(output.output_seq, seq + 1);
                seq = output.output_seq;
                parser.process(&output.data);
            }
        }
    })
    .await
    .unwrap();
    let again = handle.attach().await.unwrap();
    assert!(screen(&again.snapshot).contains("REPLY:after snapshot"));
    assert_eq!(host.process_id(), pid);
    assert_eq!(
        again.control.controller_connection,
        Some(first.connection_id)
    );
}

#[tokio::test]
async fn reconnect_before_old_disconnect_rotates_authority_and_resize_is_owner_only() {
    let directory = tempfile::tempdir().unwrap();
    let host = host(directory.path(), ECHO);
    let handle = host.handle();
    let first = wait_screen(&handle, "READY").await;
    let grant = handle.claim(first.connection_id).await.unwrap();
    let second = handle.attach().await.unwrap();
    let secret = grant.reconnect_secret;
    let fresh = handle
        .reconnect(second.connection_id, secret.clone())
        .await
        .unwrap();
    assert!(handle.reconnect(first.connection_id, secret).await.is_err());
    assert!(
        handle
            .resize(first.connection_id, grant.generation, 40, 100)
            .await
            .is_err()
    );
    drop(first);
    assert!(
        handle
            .resize(second.connection_id, fresh.generation, 30, 100)
            .await
            .unwrap()
    );
    assert!(
        !handle
            .resize(second.connection_id, fresh.generation, 30, 100)
            .await
            .unwrap()
    );
    handle
        .input(
            second.connection_id,
            fresh.generation,
            1,
            b"resumed\n".to_vec(),
        )
        .await
        .unwrap();
    let view = wait_screen(&handle, "REPLY:resumed").await;
    assert_eq!((view.snapshot.rows, view.snapshot.cols), (30, 100));
    assert_eq!(
        view.control.controller_connection,
        Some(second.connection_id)
    );
    assert!(
        view.snapshot.truncated,
        "resize snapshot does not promise scrollback"
    );
}

#[tokio::test]
async fn a_slow_page_cannot_block_pty_output_or_unboundedly_retain_history() {
    let directory = tempfile::tempdir().unwrap();
    let host = host(
        directory.path(),
        "stty -echo; printf 'READY\\r\\n'; read -r line; dd if=/dev/zero bs=8192 count=192 2>/dev/null | tr '\\000' x; printf '\\r\\nOUTPUT_DONE\\r\\n'",
    );
    let handle = host.handle();
    let mut slow = wait_screen(&handle, "READY").await;
    let grant = handle.claim(slow.connection_id).await.unwrap();
    handle
        .input(slow.connection_id, grant.generation, 1, b"go\n".to_vec())
        .await
        .unwrap();
    let after = wait_screen(&handle, "OUTPUT_DONE").await;
    assert!(after.snapshot.truncated);
    assert!(after.snapshot.replay.len() <= 1024 * 1024);
    assert!(after.retained_bytes <= 1024 * 1024);
    assert!(after.checkpoint_bytes > 0);
    // No reads occurred on the slow subscription during the full native write.
    assert!(slow.events.is_closed());
    let mut count = 0;
    while slow.events.try_recv().is_ok() {
        count += 1;
    }
    assert!(count <= CLIENT_EVENTS);
}

#[tokio::test]
async fn protocol_reply_uses_cursor_at_query_and_osc_side_effects_are_filtered() {
    let directory = tempfile::tempdir().unwrap();
    let host = host(
        directory.path(),
        "stty raw -echo; printf 'abc\\033[6nzzz\\033]52;c;c2VjcmV0\\007'; dd bs=1 count=6 of=reply.bin 2>/dev/null; printf '\\r\\nQUERY_DONE\\r\\n'",
    );
    let view = wait_screen(&host.handle(), "QUERY_DONE").await;
    assert_eq!(
        std::fs::read(directory.path().join("reply.bin")).unwrap(),
        b"\x1b[1;4R"
    );
    assert!(screen(&view.snapshot).contains("abczzz"));
    assert!(!view.snapshot.replay.windows(4).any(|part| part == b"]52;"));
}

#[tokio::test]
async fn native_exit_is_readable_and_never_restarts_a_shell() {
    let directory = tempfile::tempdir().unwrap();
    let host = host(directory.path(), "printf 'NATIVE_EXIT\\r\\n'; exit 7");
    let handle = host.handle();
    let view = wait_screen(&handle, "NATIVE_EXIT").await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let state = handle.attach().await.unwrap();
            if let Some(exit) = state.exit {
                assert_eq!(exit.code, 7);
                assert!(state.control.ended);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        handle.claim(view.connection_id).await.err().unwrap().code,
        ErrorCode::Control(ControlError::Ended)
    );
    handle.stop().await.unwrap();
    handle.stop().await.unwrap();
    assert!(screen(&handle.attach().await.unwrap().snapshot).contains("NATIVE_EXIT"));
}

#[tokio::test]
async fn screen_failure_keeps_cli_input_and_raw_output_alive_with_an_explicit_gap() {
    let directory = tempfile::tempdir().unwrap();
    let host = host(directory.path(), ECHO);
    let handle = host.handle();
    let pid = host.process_id();
    let mut page = wait_screen(&handle, "READY").await;
    let grant = handle.claim(page.connection_id).await.unwrap();
    let seq = page.snapshot.to_seq;
    handle.call(Command::FailScreen).await.unwrap();
    handle
        .resize(page.connection_id, grant.generation, 30, 39)
        .await
        .unwrap();
    handle
        .input(
            page.connection_id,
            grant.generation,
            1,
            b"after VT failure\n".to_vec(),
        )
        .await
        .unwrap();
    let mut output = Vec::new();
    let mut latest_seq = seq;
    let mut saw_fault = false;
    tokio::time::timeout(Duration::from_secs(3), async {
        while !String::from_utf8_lossy(&output).contains("REPLY:after VT failure") {
            match page.events.recv().await.unwrap() {
                TerminalEvent::Output(event) => {
                    assert_eq!(event.output_seq, latest_seq + 1);
                    latest_seq = event.output_seq;
                    output.extend_from_slice(&event.data);
                }
                TerminalEvent::Fault(fault) => {
                    assert_eq!(fault.code, ErrorCode::ScreenStateUnavailable);
                    assert_eq!(fault.run_epoch, handle.epoch());
                    assert_eq!(fault.output_seq, Some(seq));
                    saw_fault = true;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(saw_fault);
    assert_eq!(host.process_id(), pid);
    let refreshed = handle.attach().await.unwrap();
    assert!(refreshed.exit.is_none());
    assert!(!refreshed.snapshot.complete);
    assert!(refreshed.snapshot.truncated);
    assert_eq!(refreshed.snapshot.to_seq, latest_seq);
    assert_eq!(
        refreshed.fault.unwrap().code,
        ErrorCode::ScreenStateUnavailable
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("inputs.txt")).unwrap(),
        "after VT failure\n"
    );
    handle.stop().await.unwrap();
}

#[tokio::test]
async fn dropping_host_forces_only_its_uncooperative_child_to_exit() {
    let directory = tempfile::tempdir().unwrap();
    let host = host(
        directory.path(),
        "trap '' TERM; printf 'READY\\r\\n'; exec sleep 60",
    );
    wait_screen(&host.handle(), "READY").await;
    let pid = host.process_id() as libc::pid_t;
    let at = Instant::now();
    drop(host);
    assert!(at.elapsed() < Duration::from_secs(3));
    // SAFETY: signal zero only checks whether this known child still exists.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}

#[tokio::test]
async fn native_child_that_stops_reading_cannot_block_cleanup_or_replay_partial_input() {
    let directory = tempfile::tempdir().unwrap();
    let host = host(
        directory.path(),
        "stty raw -echo; printf 'READY'; exec sleep 60",
    );
    let handle = host.handle();
    let owner = wait_screen(&handle, "READY").await;
    let grant = handle.claim(owner.connection_id).await.unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        handle.input(
            owner.connection_id,
            grant.generation,
            1,
            vec![b'x'; 64 * 1024],
        ),
    )
    .await
    .expect("a full native input buffer must not block the actor")
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Control(ControlError::PtyWriteFailed));
    assert_eq!(error.source, "workbench_terminal");
    assert_eq!(error.run_epoch, handle.epoch);
    assert!(error.output_seq.is_some());
    assert_eq!(
        handle
            .input(owner.connection_id, grant.generation, 1, b"retry".to_vec())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Control(ControlError::InputSequence)
    );
    handle.stop().await.unwrap();
}
