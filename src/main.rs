#![recursion_limit = "256"]

#[cfg(test)]
mod architecture;
mod clock;
mod config;
#[allow(dead_code, unused_imports)] // Legacy Controller remains only as migration-test evidence.
mod controller;
mod credentials;
mod domain;
mod http;
mod ingest;
mod instance_lock;
mod permissions;
mod session;
mod store;
mod tailscale;
mod watcher;
mod writer;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{oneshot, watch};
use tracing::{error, info};

use crate::config::Config;
use crate::credentials::{load_or_create_token, rotate_token};
use crate::ingest::Importer;
use crate::instance_lock::InstanceLock;
use crate::store::Database;
use crate::writer::WriterHandle;

#[derive(Debug, Parser)]
#[command(name = "codex-observerd", version, about)]
struct Cli {
    #[arg(long, global = true, default_value = "observer.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Import history once and serve the read-only API and Web Viewer.
    Serve,
    /// Import all configured rollout history once.
    Import,
    /// Run read-only configuration, source and database diagnostics.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Rebuild Thread/Turn/Item projections from retained raw events.
    RebuildProjections,
    /// Preview or apply raw-event retention; projections and dedupe tombstones remain.
    Retention {
        /// Apply deletions. Without this flag the command is a dry run.
        #[arg(long)]
        apply: bool,
    },
    /// Export one redacted Observer thread copy without modifying Codex data.
    Export {
        #[arg(long)]
        thread: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Print the reusable Viewer pairing link for the running server that uses this config.
    Open,
    /// Permanently suppress and delete one local Observer thread copy.
    Purge {
        #[arg(long)]
        thread: String,
        /// Confirm that only the Observer copy, never the Codex store, is targeted.
        #[arg(long)]
        observer_copy_only: bool,
        /// Non-interactive destructive-operation confirmation.
        #[arg(long)]
        yes: bool,
    },
    #[command(hide = true)]
    OwnedAppServerGuard {
        #[arg(long)]
        codex_executable: PathBuf,
        #[arg(long)]
        cwd: PathBuf,
        #[arg(long)]
        socket: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    permissions::set_private_umask();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "codex_local_observer=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();
    if let Command::OwnedAppServerGuard {
        codex_executable,
        cwd,
        socket,
    } = &cli.command
    {
        return session::run_owned_app_server_guard(codex_executable, cwd, socket);
    }
    let config = Config::load(&cli.config)?;
    config.validate()?;
    if let Command::Doctor { json } = &cli.command {
        let mut report = Database::doctor_read_only(&config)?;
        let runtime_status = if session::codex_cli_available().is_some() {
            "ready"
        } else {
            report.status = "degraded".into();
            "codex_cli_unavailable"
        };
        for source in &mut report.sources {
            source.session_runtime_status = runtime_status.into();
        }
        print_doctor(report, *json)?;
        return Ok(());
    }
    if matches!(cli.command, Command::Open) {
        if !InstanceLock::pairing_ready(config.database_path())? {
            anyhow::bail!(
                "no pairing-ready Observer instance matches config {}; start `serve`, wait for startup, or pass the same --config used by the running server",
                cli.config.display()
            );
        }
        let token = load_or_create_token(&config.server.bearer_token_file)?;
        println!(
            "{}",
            http::generate_pairing_url(config.server.bind, &token)?
        );
        return Ok(());
    }
    if let Command::Export { thread, output } = &cli.command {
        let output = safe_export_output(&config, output)?;
        let database = Database::open_read_only_with_blobs(
            config.database_path(),
            &config.storage.blob_dir,
            config.capture.inline_blob_bytes,
        )?;
        let report = database.export_thread(thread, &output)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    if let Command::Purge {
        observer_copy_only,
        yes,
        ..
    } = &cli.command
    {
        if !observer_copy_only {
            anyhow::bail!(
                "purge requires --observer-copy-only; Codex source data is never modified"
            );
        }
        if !yes {
            anyhow::bail!("purge requires explicit --yes confirmation");
        }
    }
    let mut instance_lock = InstanceLock::acquire(config.database_path())?;
    info!(path = %instance_lock.path().display(), "Observer writer lock acquired");
    let server_token = if matches!(cli.command, Command::Serve) {
        let token = rotate_token(&config.server.bearer_token_file)?;
        println!(
            "Starting Local Viewer on http://{}; pairing link will appear when it is ready.",
            config.server.bind
        );
        Some(token)
    } else {
        None
    };
    let database = Arc::new(Database::open_with_blobs(
        config.database_path(),
        &config.storage.blob_dir,
        config.capture.inline_blob_bytes,
    )?);
    database.migrate()?;
    let gateway_recovery = database.recover_gateway_after_restart()?;
    for path in &gateway_recovery.image_paths {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("remove stale image {path}")),
        }
    }
    if gateway_recovery.closed_epochs > 0
        || gateway_recovery.failed_before_dispatch > 0
        || gateway_recovery.outcome_unknown > 0
    {
        info!(
            closed_epochs = gateway_recovery.closed_epochs,
            failed_before_dispatch = gateway_recovery.failed_before_dispatch,
            outcome_unknown = gateway_recovery.outcome_unknown,
            removed_images = gateway_recovery.image_paths.len(),
            "Gateway restart recovery completed without replay"
        );
    }
    let session_recovery = database.recover_sessions_after_restart()?;
    if session_recovery.orphaned_workers > 0
        || session_recovery.orphaned_thread_leases > 0
        || session_recovery.orphaned_attachments > 0
        || session_recovery.orphaned_input_leases > 0
    {
        info!(
            orphaned_workers = session_recovery.orphaned_workers,
            orphaned_thread_leases = session_recovery.orphaned_thread_leases,
            orphaned_attachments = session_recovery.orphaned_attachments,
            orphaned_input_leases = session_recovery.orphaned_input_leases,
            "Session Kernel restart recovery completed without process adoption or replay"
        );
    }
    let expired_images = database.sweep_expired_image_uploads(crate::clock::now_ms())?;
    if expired_images > 0 {
        info!(files = expired_images, "removed expired staged images");
    }
    let writer = WriterHandle::start(
        database.clone(),
        config.capture.ingest_queue_events,
        128,
        config.capture.api_consumer_queue_events,
    )?;
    let swept = database.sweep_orphan_blobs(60 * 60 * 1000)?;
    if swept > 0 {
        info!(files = swept, "removed orphan blob files");
    }

    match cli.command {
        Command::Import => {
            let report =
                Importer::new_with_writer(&config, database.as_ref(), &writer)?.import_all()?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Doctor { .. } => unreachable!("doctor exits before writer initialization"),
        Command::RebuildProjections => {
            let rebuilt = database.rebuild_projections()?;
            println!("rebuilt {rebuilt} events");
        }
        Command::Retention { apply } => {
            let report = database.run_retention(
                config.storage.raw_event_retention_days,
                config.storage.delta_retention_days,
                config.storage.blob_retention_days,
                apply,
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Export { .. } => unreachable!("export exits before writer initialization"),
        Command::Open => unreachable!("open exits before writer initialization"),
        Command::Purge {
            thread,
            observer_copy_only: _,
            yes: _,
        } => {
            let report = database.purge_thread(&thread)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Serve => {
            let (_watcher, mut rescan_hints) = watcher::RolloutWatcher::start(&config)?;
            let (shutdown_sender, shutdown_receiver) = watch::channel(false);
            let fingerprint_key =
                credentials::load_or_create_key(&config.storage.fingerprint_key_file)?;
            let session_kernel = session::SessionKernel::from_config(
                &config,
                database.clone(),
                writer.clone(),
                fingerprint_key,
            )?;
            let signal_sender = shutdown_sender.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    let _ = signal_sender.send(true);
                }
            });

            let (http_ready_sender, http_ready_receiver) = oneshot::channel();
            let http_handle = tokio::spawn(http::serve(
                config.clone(),
                database.clone(),
                writer.clone(),
                session_kernel.clone(),
                server_token.expect("Serve initialized one startup token"),
                http::ServeLifecycle {
                    ready: http_ready_sender,
                    shutdown: shutdown_receiver.clone(),
                },
            ));
            let http_ready = match http_ready_receiver.await {
                Ok(ready) => ready,
                Err(_) => {
                    return http_handle
                        .await
                        .context("Observer Web Viewer task failed before readiness")?;
                }
            };
            wait_for_local_viewer(http_ready.bound_address).await?;
            instance_lock.mark_pairing_ready()?;
            println!("Local Viewer: {}", http_ready.viewer_url);
            if let Some(viewer_url) = http_ready.tailscale_viewer_url {
                println!("Tailscale Viewer: {viewer_url}");
            }

            let initial =
                Importer::new_with_writer(&config, database.as_ref(), &writer)?.import_all()?;
            info!(
                files = initial.files_scanned,
                events = initial.events_inserted,
                "initial import complete"
            );

            let second =
                Importer::new_with_writer(&config, database.as_ref(), &writer)?.import_all()?;
            info!(
                files = second.files_scanned,
                events = second.events_inserted,
                "post-watcher reconciliation complete"
            );

            let scan_config = config.clone();
            let scan_db = database.clone();
            let scan_writer = writer.clone();
            tokio::spawn(async move {
                let interval = scan_config.minimum_scan_interval();
                let mut ticker = tokio::time::interval(Duration::from_secs(interval));
                ticker.tick().await;
                loop {
                    tokio::select! {
                        _ = ticker.tick() => {}
                        hint = rescan_hints.recv() => {
                            let Some(path) = hint else { break };
                            tracing::debug!(path = %path.display(), "rollout watcher requested rescan");
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            while rescan_hints.try_recv().is_ok() {}
                        }
                    }
                    let cfg = scan_config.clone();
                    let db = scan_db.clone();
                    let writer = scan_writer.clone();
                    match tokio::task::spawn_blocking(move || {
                        Importer::new_with_writer(&cfg, db.as_ref(), &writer)?.import_all()
                    })
                    .await
                    {
                        Ok(Ok(report)) => info!(
                            files = report.files_scanned,
                            events = report.events_inserted,
                            "periodic scan complete"
                        ),
                        Ok(Err(err)) => error!(error = %err, "periodic scan failed"),
                        Err(err) => error!(error = %err, "periodic scanner task failed"),
                    }
                }
            });
            http_handle
                .await
                .context("Observer Web Viewer task failed")??;
            let _ = shutdown_sender.send(true);
            session_kernel.shutdown().await;
        }
        Command::OwnedAppServerGuard { .. } => {
            unreachable!("owned App Server guard exits before configuration loading")
        }
    }
    Ok(())
}

async fn wait_for_local_viewer(address: std::net::SocketAddr) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async move {
        let mut stream = TcpStream::connect(address).await?;
        stream
            .write_all(
                format!("GET / HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await?;
        let mut response = [0_u8; 64];
        let read = stream.read(&mut response).await?;
        let status = std::str::from_utf8(&response[..read]).context("read Viewer probe status")?;
        if !status.starts_with("HTTP/1.1 200 ") && !status.starts_with("HTTP/1.0 200 ") {
            anyhow::bail!("Viewer readiness probe returned a non-200 response");
        }
        Result::<()>::Ok(())
    })
    .await
    .context("timed out waiting for Local Viewer readiness")??;
    Ok(())
}

fn print_doctor(report: crate::domain::model::DoctorReport, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("status: {}", report.status);
        println!("database: {}", report.database);
        for source in report.sources {
            println!(
                "source {}: {} ({}; session-runtime={})",
                source.name, source.status, source.path, source.session_runtime_status
            );
        }
        println!("checks: {}", serde_json::to_string(&report.checks)?);
    }
    Ok(())
}

fn safe_export_output(config: &Config, output: &std::path::Path) -> Result<PathBuf> {
    let absolute = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()?.join(output)
    };
    let parent = absolute
        .parent()
        .context("export output must have a parent directory")?;
    let file_name = absolute
        .file_name()
        .context("export output must have a file name")?;
    let resolved = std::fs::canonicalize(parent)
        .with_context(|| format!("resolve export directory {}", parent.display()))?
        .join(file_name);
    if config.sources.iter().any(|source| {
        let source_home =
            std::fs::canonicalize(&source.codex_home).unwrap_or_else(|_| source.codex_home.clone());
        resolved.starts_with(source_home)
    }) {
        anyhow::bail!("export output must not be inside a Codex source");
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn viewer_readiness_requires_a_successful_http_response() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let mut request = [0_u8; 256];
            let _ = stream.read(&mut request).await?;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await?;
            Result::<()>::Ok(())
        });

        wait_for_local_viewer(address).await?;
        server.await??;
        Ok(())
    }

    #[test]
    fn export_output_cannot_target_codex_source() -> Result<()> {
        let temp = TempDir::new()?;
        let mut config = Config::default();
        config.sources[0].codex_home = temp.path().join("codex-home");
        std::fs::create_dir_all(&config.sources[0].codex_home)?;
        let inside = config.sources[0].codex_home.join("export.json");
        assert!(safe_export_output(&config, &inside).is_err());
        let outside = temp.path().join("export.json");
        assert_eq!(
            safe_export_output(&config, &outside)?,
            std::fs::canonicalize(temp.path())?.join("export.json")
        );
        Ok(())
    }
}
