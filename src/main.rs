mod api;
mod config;
mod db;
mod ingest;
mod instance_lock;
mod model;
mod redact;
mod watcher;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
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
            api::serve(config, database).await?;
        }
    }
    Ok(())
}
