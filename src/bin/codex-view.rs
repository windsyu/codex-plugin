use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use codex_local_observer::workbench::config::{Overrides, Prepared};
use codex_local_observer::workbench::launch::{
    LaunchOptions, WorkbenchRun, native_home, open_browser, read_entry,
};
use codex_local_observer::workbench::paths::WorkbenchPaths;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(about = "Run the official Codex CLI and a local live workbench in this directory")]
struct Args {
    /// Keep the browser closed; use the private entry with `codex-view open`.
    #[arg(long)]
    no_open: bool,
    /// Native $CODEX_HOME/<name>.config.toml profile; settings remain with CLI.
    #[arg(long)]
    profile: Option<String>,
    /// Validated model-routing adapter (does not select a different model).
    #[arg(long)]
    provider_profile: Option<String>,
    /// Existing installed official CLI; never downloaded or upgraded here.
    #[arg(long)]
    codex_bin: Option<PathBuf>,
    /// Workbench JSON directory; defaults to ~/.codex-web/config.
    #[arg(long)]
    config_dir: Option<PathBuf>,
    /// Explicitly resume this native thread; no automatic resume or input replay.
    #[arg(long)]
    resume: Option<uuid::Uuid>,
    /// New private pairing file for local integrations. Removed on clean exit.
    #[arg(long)]
    entry_file: Option<PathBuf>,
    /// Private history directory; defaults to ~/.codex-web/history.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    /// Open an already running workbench from its private entry file.
    Open { entry_file: PathBuf },
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args = Args::parse();
    if let Err(error) = run(args).await {
        eprintln!("codex-view: {error:#}");
        std::process::exit(1);
    }
}
async fn run(args: Args) -> Result<()> {
    if let Some(Action::Open { entry_file }) = args.command {
        let entry = read_entry(&entry_file)?;
        return open_browser(&entry.url).await;
    }
    let cwd = std::env::current_dir()?;
    let home = native_home()?;
    let paths = WorkbenchPaths::discover()?;
    let config_path = args
        .config_dir
        .clone()
        .unwrap_or_else(|| paths.config_dir())
        .join("config.json");
    let prepared = Prepared::load(
        &home,
        &cwd,
        &paths,
        args.config_dir.as_deref(),
        Overrides {
            open_browser: args.no_open.then_some(false),
            codex_bin: args.codex_bin,
            profile: args.profile,
            provider_profile: args.provider_profile,
            data_dir: args.data_dir,
        },
    )
    .map_err(|error| anyhow::anyhow!("配置 {}：{error}", config_path.display()))?;
    let config = prepared.effective.clone();
    // Register shutdown handlers before starting the native child.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let workbench = WorkbenchRun::start_configured(
        LaunchOptions {
            cwd,
            home,
            workbench_paths: paths,
            executable: PathBuf::from(&config.launch.codex_bin),
            native_profile: config.launch.profile,
            resume: args.resume,
            entry_file: args.entry_file,
            data_dir: Some(prepared.data_dir.clone()),
        },
        prepared,
    )
    .await?;
    println!(
        "{}",
        serde_json::json!({"stage":"ready","address":format!("http://{}",workbench.web.address()),"runEpoch":workbench.epoch,"cliPid":workbench.process_id(),"cliVersion":workbench.cli_version,"entryFile":workbench.entry_file()})
    );
    if config.launch.open_browser && open_browser(&workbench.web.bootstrap_url()).await.is_err() {
        eprintln!("浏览器未能自动打开；可用 codex-view open <entryFile> 打开本次工作台。");
    }
    if !config.launch.open_browser {
        eprintln!(
            "使用 codex-view open <entryFile> 打开配对页面。Ctrl-C 结束本次工作台；关闭网页不会停止 CLI。"
        );
    }
    let mut health = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            _=interrupt.recv()=>break,
            _=terminate.recv()=>break,
            _=hangup.recv()=>break,
            _=health.tick()=>ensure!(workbench.healthy(),"workbench runtime stopped unexpectedly (epoch {})",workbench.epoch),
        }
    }
    drop(workbench);
    Ok(())
}
