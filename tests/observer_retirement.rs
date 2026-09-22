//! The retired control configuration must never execute a CLI or recover old ledgers.
//! All homes, source data and databases in this test are synthetic and temporary.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;

struct OwnedServer(Child);
impl Drop for OwnedServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn ledger(connection: &Connection) -> Result<Vec<String>> {
    [
        "gateway_commands",
        "command_transitions",
        "control_audit",
        "worker_connection_epochs",
    ]
    .into_iter()
    .map(|table| {
        let mut statement = connection.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|index| row.get::<_, rusqlite::types::Value>(index))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(format!("{rows:?}"))
    })
    .collect()
}

#[tokio::test]
async fn observer_with_retired_config_preserves_control_ledger_and_never_launches_cli() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let native = root.join("native");
    fs::create_dir_all(native.join("sessions"))?;
    fs::create_dir_all(root.join("bin"))?;
    let fake = root.join("bin/codex");
    fs::write(
        &fake,
        "#!/bin/sh\nprintf invoked > \"$HOME/cli-was-invoked\"\nexit 42\n",
    )?;
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700))?;
    let db_path = root.join("observer.sqlite");
    let connection = Connection::open(&db_path)?;
    let mut migrations = fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    migrations.sort();
    for migration in migrations {
        if migration.extension().is_some_and(|value| value == "sql") {
            connection.execute_batch(&fs::read_to_string(migration)?)?;
        }
    }
    connection.execute_batch("INSERT INTO gateway_commands(command_id,principal_id,capability,idempotency_key,payload_hash,source_id,source_epoch,input_summary_json,state,created_at_ms,updated_at_ms)
        VALUES ('retired-command','local_bearer','turn.start','fixture-key','hash','source','epoch','{}','received',1,1);
        INSERT INTO command_transitions(command_id,from_state,to_state,occurred_at_ms,details_summary_json) VALUES ('retired-command',NULL,'received',1,'{}');
        INSERT INTO control_audit(command_id,principal_id,capability,source_id,source_epoch,decision,outcome,payload_hash,input_summary_json,occurred_at_ms)
        VALUES ('retired-command','local_bearer','turn.start','source','epoch','received','pending','hash','{}',1);
        INSERT INTO worker_connection_epochs(worker_id,connection_epoch,source_id,source_epoch,state,opened_at_ms)
        VALUES ('old-worker','connection','source','epoch','open',1);")?;
    let before = ledger(&connection)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    drop(listener);
    let config = root.join("observer.toml");
    fs::write(
        &config,
        format!(
            r#"
[server]
bind = "{address}"
bearer_token_file = "token"
[storage]
database = "observer.sqlite"
blob_dir = "blobs"
fingerprint_key_file = "fingerprint.key"
[controller]
enabled = true
session_kernel = "preview"
session_fixture_cli = "bin/codex"
[[sources]]
name = "synthetic"
codex_home = "native"
scan_interval_seconds = 30
"#
        ),
    )?;
    let output = fs::File::create(root.join("observer.log"))?;
    let mut server = OwnedServer(
        Command::new(env!("CARGO_BIN_EXE_codex-observerd"))
            .args(["--config", config.to_str().unwrap(), "serve"])
            .env("HOME", root)
            .env("USERPROFILE", root)
            .env("CODEX_HOME", &native)
            .env("PATH", root.join("bin"))
            .env("RUST_LOG", "warn")
            .stdout(output.try_clone()?)
            .stderr(output)
            .stdin(Stdio::null())
            .spawn()?,
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(500))
        .build()?;
    let base = format!("http://{address}");
    let deadline = Instant::now() + Duration::from_secs(15);
    let token = loop {
        assert!(
            server.0.try_wait()?.is_none(),
            "Observer exited during startup"
        );
        if let Ok(token) = fs::read_to_string(root.join("token")) {
            let token = token.trim().to_string();
            if let Ok(response) = client
                .get(format!("{base}/v1/health"))
                .bearer_auth(&token)
                .send()
                .await
                && response.status().is_success()
            {
                break token;
            }
        }
        anyhow::ensure!(Instant::now() < deadline, "Observer did not become ready");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    for (method, route) in [
        (reqwest::Method::GET, "/v2/session-sources"),
        (reqwest::Method::POST, "/v2/sessions/fake"),
        (reqwest::Method::POST, "/v2/sessions"),
        (reqwest::Method::POST, "/v2/commands"),
        (reqwest::Method::GET, "/v2/sessions/old/terminal"),
    ] {
        let response = client
            .request(method, format!("{base}{route}"))
            .bearer_auth(&token)
            .header("Origin", &base)
            .body("{}")
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND, "{route}");
    }
    let response = client
        .get(format!("{base}/v1/threads"))
        .bearer_auth(&token)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let value: Value = serde_json::from_slice(&response.bytes().await?)?;
    assert!(value["data"].is_array());
    assert!(
        !root.join("cli-was-invoked").exists(),
        "Observer executed a retired CLI path"
    );
    assert_eq!(
        ledger(&connection)?,
        before,
        "startup or requests rewrote the old control ledger"
    );
    assert_eq!(
        connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?,
        20
    );
    assert!(
        fs::read_to_string(root.join("observer.log"))?
            .contains("[controller] is retired and ignored")
    );
    unsafe {
        libc::kill(server.0.id() as i32, libc::SIGINT);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = server.0.try_wait()? {
            anyhow::ensure!(status.success(), "Observer shutdown failed");
            break;
        }
        anyhow::ensure!(Instant::now() < deadline, "Observer shutdown timed out");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(ledger(&connection)?, before, "shutdown rewrote old records");
    let diagnostic = Command::new(env!("CARGO_BIN_EXE_codex-observerd"))
        .args([
            "--config",
            config.to_str().context("config path")?,
            "doctor",
            "--json",
        ])
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("CODEX_HOME", &native)
        .env("PATH", root.join("bin"))
        .output()?;
    assert!(diagnostic.status.success());
    assert!(
        !root.join("cli-was-invoked").exists(),
        "history diagnostics invoked Codex"
    );
    Ok(())
}
