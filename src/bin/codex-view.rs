use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use codex_local_observer::workbench::application::{self, Acquisition, Application, InstanceLock};
use codex_local_observer::workbench::config::{Overrides, Prepared};
use codex_local_observer::workbench::launch::{native_home, open_browser, read_entry};
use codex_local_observer::workbench::paths::WorkbenchPaths;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(about = "Open local project history; explicitly select a project to start Codex CLI")]
struct Args {
    /// Keep the browser closed; use the private entry with `codex-view open`.
    #[arg(long)]
    no_open: bool,
    /// Explicitly start a new conversation in this existing project directory.
    #[arg(long)]
    project: Option<PathBuf>,
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
    let home =
        codex_local_observer::workbench::config::location_for_application(&native_home()?, &cwd)?;
    let paths = WorkbenchPaths::discover()?;
    let config_path = args
        .config_dir
        .clone()
        .unwrap_or_else(|| paths.config_dir())
        .join("config.json");
    let overrides = Overrides {
        open_browser: args.no_open.then_some(false),
        codex_bin: args.codex_bin,
        profile: args.profile,
        provider_profile: args.provider_profile,
        data_dir: args.data_dir,
    };
    let prepared = Prepared::load_application(
        &home,
        &cwd,
        &paths,
        args.config_dir.as_deref(),
        overrides.clone(),
    )
    .map_err(|error| anyhow::anyhow!("配置 {}：{error}", config_path.display()))?;
    let config = prepared.effective.clone();
    let project = args
        .project
        .map(|p| if p.is_absolute() { p } else { cwd.join(p) })
        .or_else(|| args.resume.map(|_| cwd.clone()));
    let mut request = application::request(
        &overrides,
        project,
        args.resume,
        codex_local_observer::workbench::config::location_for_application(&home, &cwd)?,
    );
    // Prepared normalizes explicit binary paths relative to the invocation cwd.
    if overrides.codex_bin.is_some() {
        request.codex_bin = Some(config.launch.codex_bin.clone());
    }
    let lock = match InstanceLock::acquire(&paths, prepared.config_dir(), &prepared.data_dir)? {
        Acquisition::Owner(lock) => lock,
        Acquisition::Existing(path) => {
            ensure!(
                args.entry_file.is_none() || args.entry_file.as_ref() == Some(&path),
                "instance already running; use its existing entry file"
            );
            let mut entry = None;
            for _ in 0..20 {
                match read_entry(&path) {
                    Ok(value) => {
                        entry = Some(value);
                        break;
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
            let entry = entry.ok_or_else(|| anyhow::anyhow!("application is starting or its private entry is unavailable; retry without deleting locks"))?;
            let run = application::connect(&entry, request).await?;
            println!(
                "{}",
                serde_json::json!({"stage":"reused","instanceId":entry.instance_id,"address":entry.address,"entryFile":path,"runEpoch":run.as_ref().map(|r| r.run_id),"cliPid":run.as_ref().map(|r| r.cli_pid)})
            );
            if config.launch.open_browser {
                let mut url = reqwest::Url::parse(&entry.url)?;
                if let Some(run) = run {
                    url.set_query(Some(&format!("run={}", run.run_id)));
                }
                open_browser(url.as_str()).await?;
            }
            return Ok(());
        }
    };
    // Register shutdown handlers before starting the native child.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let application = Application::start(prepared, home, paths, lock, args.entry_file.as_deref())?;
    // Publish the HTTP/entry before explicit CLI validation. A failed launch leaves
    // the application usable; Ctrl-C still owns the complete application lifetime.
    println!(
        "{}",
        serde_json::json!({"stage":"ready","instanceId":application.instance_id,"address":application.web.origin(),"entryFile":application.entry_file()})
    );
    let mut target = None;
    if request.project.is_some() {
        match application::connect(&application.entry, request).await {
            Ok(run) => {
                if let Some(run) = run {
                    target = Some(run.run_id);
                    println!(
                        "{}",
                        serde_json::json!({"stage":"run-ready","instanceId":application.instance_id,"runEpoch":run.run_id,"cliPid":run.cli_pid,"cliVersion":run.cli_version})
                    );
                }
            }
            Err(error) => eprintln!("启动未成功：{error}。历史首页仍可打开。"),
        }
    }
    if config.launch.open_browser
        && open_browser(&application.web.bootstrap_url(target))
            .await
            .is_err()
    {
        eprintln!("浏览器未能自动打开；可用 codex-view open <entryFile> 打开首页。");
    }
    if !config.launch.open_browser {
        eprintln!(
            "使用 codex-view open <entryFile> 打开页面。Ctrl-C 关闭应用及其 CLI；关闭网页不会停止 CLI。"
        );
    }
    let mut health = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            _=interrupt.recv()=>break,
            _=terminate.recv()=>break,
            _=hangup.recv()=>break,
            _=health.tick()=>ensure!(application.healthy(),"application runtime stopped unexpectedly"),
        }
    }
    drop(application);
    Ok(())
}
