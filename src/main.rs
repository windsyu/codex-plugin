mod api;
mod config;
mod db;
mod ingest;
mod instance_lock;
mod live;
mod model;
mod redact;
mod watcher;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::sync::watch;
use tracing::{error, info};

use crate::config::Config;
use crate::db::Database;
use crate::ingest::Importer;
use crate::instance_lock::InstanceLock;

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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "codex_local_observer=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    config.validate()?;
    let instance_lock = InstanceLock::acquire(config.database_path())?;
    info!(path = %instance_lock.path().display(), "Observer writer lock acquired");
    let database = Arc::new(Database::open(config.database_path())?);
    database.migrate()?;

    match cli.command {
        Command::Import => {
            let report = Importer::new(&config, database.as_ref())?.import_all()?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Doctor { json } => {
            let report = database.doctor(&config)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("status: {}", report.status);
                println!("database: {}", report.database);
                for source in report.sources {
                    println!(
                        "source {}: {} ({})",
                        source.name, source.status, source.path
                    );
                }
            }
        }
        Command::RebuildProjections => {
            let rebuilt = database.rebuild_projections()?;
            println!("rebuilt {rebuilt} events");
        }
        Command::Retention { apply } => {
            let report = database.run_retention(config.storage.raw_event_retention_days, apply)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Serve => {
            let initial = Importer::new(&config, database.as_ref())?.import_all()?;
            info!(
                files = initial.files_scanned,
                events = initial.events_inserted,
                "initial import complete"
            );

            let (_watcher, mut rescan_hints) = watcher::RolloutWatcher::start(&config)?;
            let second = Importer::new(&config, database.as_ref())?.import_all()?;
            info!(
                files = second.files_scanned,
                events = second.events_inserted,
                "post-watcher reconciliation complete"
            );
            let (shutdown_sender, shutdown_receiver) = watch::channel(false);
            let live_handles =
                live::spawn_enabled(&config, database.clone(), shutdown_receiver.clone())?;

            let scan_config = config.clone();
            let scan_db = database.clone();
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
                    match tokio::task::spawn_blocking(move || {
                        Importer::new(&cfg, db.as_ref())?.import_all()
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
            let signal_sender = shutdown_sender.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    let _ = signal_sender.send(true);
                }
            });
            api::serve(config, database, shutdown_receiver).await?;
            let _ = shutdown_sender.send(true);
            for handle in live_handles {
                let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
            }
        }
    }
    Ok(())
}
