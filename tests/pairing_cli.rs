//! Pairing output uses synthetic capabilities and fully isolated user directories.
#![cfg(unix)]

use codex_local_observer::workbench::application::{Acquisition, InstanceLock, service};
use codex_local_observer::workbench::launch::BrowserEntry;
use codex_local_observer::workbench::paths::WorkbenchPaths;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn command(home: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_codex-view"));
    command
        .current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CODEX_HOME", home.join("native"));
    command
}

fn entry(address: &str) -> BrowserEntry {
    let run = uuid::Uuid::new_v4();
    BrowserEntry {
        format: "codex-view-entry-v2".into(),
        instance_id: Some(uuid::Uuid::new_v4()),
        run_id: Some(run),
        address: address.into(),
        url: format!("{address}/?run={run}#pair={}", "a".repeat(64)),
        ..Default::default()
    }
}

fn write_entry(path: &Path, entry: &BrowserEntry) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    serde_json::to_writer(file, entry).unwrap();
}

#[tokio::test]
async fn explicit_entry_prints_only_the_validated_pairing_url() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("private entry.json");
    let entry = entry("http://127.0.0.1:12345");
    write_entry(&path, &entry);
    for relay in [false, true] {
        let mut command = command(home.path());
        command.arg("open").arg(&path).arg("--print-url");
        if relay {
            command.arg("--relay");
        }
        let result = command.output().await.unwrap();
        assert!(result.status.success());
        assert!(result.stderr.is_empty());
        let expected = if relay {
            format!("{}/relay#pair={}", entry.address, "a".repeat(64))
        } else {
            entry.url.clone()
        };
        assert_eq!(
            String::from_utf8(result.stdout).unwrap(),
            format!("{expected}\n")
        );
    }
    assert!(!home.path().join(".codex-web").exists());
    assert!(!home.path().join("native").exists());
}

#[tokio::test]
async fn unsafe_entry_and_missing_instance_fail_without_printing_capabilities() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("entry.json");
    let mut entry = entry("http://127.0.0.1:12345");
    write_entry(&path, &entry);
    for print in [false, true] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut cmd = command(home.path());
        cmd.arg("open").arg(&path);
        if print {
            cmd.arg("--print-url");
        }
        let result = cmd.output().await.unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&result.stderr).contains(&"a".repeat(64)));
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    entry.url = entry.url.replace("127.0.0.1", "example.invalid");
    std::fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
    let result = command(home.path())
        .arg("open")
        .arg(&path)
        .arg("--print-url")
        .output()
        .await
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&result.stderr).contains(&"a".repeat(64)));
    let result = command(home.path())
        .args(["open", "--print-url"])
        .output()
        .await
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("workbench is not running"));
}

#[tokio::test]
async fn discovered_entry_requires_live_instance_identity_before_printing() {
    let home = tempfile::tempdir().unwrap();
    let paths = WorkbenchPaths::from_user_home(home.path()).unwrap();
    let config = paths.config_dir();
    let data = paths.history_dir();
    let Acquisition::Owner(lock) = InstanceLock::acquire(&paths, &config, &data).unwrap() else {
        panic!("fixture must own its private instance lock")
    };
    let manager = service::Manager::new(&paths, &config, &data).unwrap();
    assert_eq!(lock.path(), manager.directory_path().join("instance.json"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let entry = entry(&format!("http://{}", listener.local_addr().unwrap()));
    write_entry(&lock.path(), &entry);
    let server_entry = entry.clone();
    let server = tokio::spawn(async move {
        for matching in [true, false] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let read = stream.read(&mut buffer).await.unwrap();
                assert_ne!(read, 0);
                request.extend_from_slice(&buffer[..read]);
                if let Some(split) = request.windows(4).position(|w| w == b"\r\n\r\n")
                    && serde_json::from_slice::<serde_json::Value>(&request[split + 4..]).is_ok()
                {
                    break;
                }
                assert!(request.len() < 16 * 1024);
            }
            assert!(request.starts_with(b"POST /workbench/v1/application/control "));
            let body = serde_json::json!({
                "instanceId": if matching { server_entry.instance_id.unwrap() } else { uuid::Uuid::new_v4() },
                "processId": 1, "serviceLabel": null, "stopping": false, "activeRuns": 0, "relay": null
            }).to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    for matching in [true, false] {
        let result = command(home.path())
            .args(["open", "--print-url"])
            .output()
            .await
            .unwrap();
        assert_eq!(result.status.success(), matching);
        if matching {
            assert_eq!(
                String::from_utf8(result.stdout).unwrap(),
                format!("{}\n", entry.url)
            );
            assert!(result.stderr.is_empty());
        } else {
            assert!(result.stdout.is_empty());
            assert!(
                String::from_utf8_lossy(&result.stderr).contains("application instance changed")
            );
            assert!(!String::from_utf8_lossy(&result.stderr).contains(&"a".repeat(64)));
        }
    }
    server.await.unwrap();
}
