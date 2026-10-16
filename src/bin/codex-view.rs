use anyhow::{Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use codex_local_observer::workbench::application::{self, Acquisition, Application, InstanceLock};
use codex_local_observer::workbench::application::{management, service};
use codex_local_observer::workbench::config::{Overrides, Prepared};
use codex_local_observer::workbench::launch::{native_home, open_browser, read_entry};
use codex_local_observer::workbench::paths::WorkbenchPaths;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "codex-view",
    version,
    about = "Open local project history; explicitly select a project to start Codex CLI"
)]
struct Args {
    /// Keep the browser closed; use the private entry with `codex-view open`.
    #[arg(long, global = true)]
    no_open: bool,
    /// Explicitly start a new conversation in this existing project directory.
    #[arg(long)]
    project: Option<PathBuf>,
    /// Native $CODEX_HOME/<name>.config.toml profile; settings remain with CLI.
    #[arg(long, global = true)]
    profile: Option<String>,
    /// Validated model-routing adapter (does not select a different model).
    #[arg(long, global = true)]
    provider_profile: Option<String>,
    /// Existing installed official CLI; never downloaded or upgraded here.
    #[arg(long, global = true)]
    codex_bin: Option<PathBuf>,
    /// Workbench JSON directory; defaults to ~/.codex-web/config.
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,
    /// Explicitly resume this native thread; no automatic resume or input replay.
    #[arg(long)]
    resume: Option<uuid::Uuid>,
    /// New private pairing file for local integrations. Removed on clean exit.
    #[arg(long)]
    entry_file: Option<PathBuf>,
    /// Private history directory; defaults to ~/.codex-web/history.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, hide = true)]
    managed_service: Option<String>,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    /// Start the background workbench without starting a project.
    Start,
    /// Show the current workbench and relay connection status.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Open the current workbench or an explicit private entry file.
    Open {
        entry_file: Option<PathBuf>,
        #[arg(long)]
        relay: bool,
    },
    /// Fully stop this workbench and all CLI processes it owns.
    Stop,
    /// Enable/disable startup at the next macOS login.
    Autostart { mode: AutoStart },
    #[command(hide = true)]
    ServiceMaintenance {
        #[arg(long)]
        remove: bool,
    },
}
#[derive(Clone, Copy, ValueEnum)]
enum AutoStart {
    On,
    Off,
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args = Args::parse();
    if let Err(error) = run(args).await {
        eprintln!("codex-view: {error:#}");
        std::process::exit(1);
    }
}
async fn run(mut args: Args) -> Result<()> {
    let action = args.command.take();
    if let Some(Action::Open {
        entry_file: Some(entry_file),
        relay,
    }) = &action
    {
        let entry = read_entry(entry_file)?;
        return open_entry(&entry, *relay).await;
    }
    let cwd = std::env::current_dir()?;
    let home =
        codex_local_observer::workbench::config::location_for_application(&native_home()?, &cwd)?;
    let paths = WorkbenchPaths::discover()?;
    if let Some(Action::ServiceMaintenance { remove }) = action {
        return service::installation_guard(&paths, remove).await;
    }
    if action.is_some() {
        ensure!(
            args.project.is_none()
                && args.resume.is_none()
                && args.entry_file.is_none()
                && args.managed_service.is_none(),
            "service commands cannot implicitly start a project or use a custom entry"
        );
    }
    // Lifecycle access must survive a damaged current configuration. Discovery
    // uses the private registration or an explicitly supplied config/data scope.
    if matches!(
        &action,
        Some(
            Action::Status { .. }
                | Action::Stop
                | Action::Open { .. }
                | Action::Autostart {
                    mode: AutoStart::Off
                }
        )
    ) {
        let manager = service::Manager::lookup(
            &paths,
            args.config_dir.as_deref(),
            args.data_dir.as_deref(),
            &cwd,
        )?;
        return existing_action(action.unwrap(), &manager, home).await;
    }
    let config_path = args
        .config_dir
        .clone()
        .unwrap_or_else(|| paths.config_dir())
        .join("config.json");
    let overrides = Overrides {
        open_browser: args.no_open.then_some(false),
        codex_bin: args.codex_bin.clone(),
        profile: args.profile.clone(),
        provider_profile: args.provider_profile.clone(),
        data_dir: args.data_dir.clone(),
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
    let manager = service::Manager::new(&paths, prepared.config_dir(), &prepared.data_dir)?;
    let mut service_overrides = overrides.clone();
    if service_overrides.codex_bin.is_some() {
        service_overrides.codex_bin = Some(config.launch.codex_bin.clone().into());
    }
    if let Some(action) = action {
        ensure!(
            args.project.is_none()
                && args.resume.is_none()
                && args.entry_file.is_none()
                && args.managed_service.is_none(),
            "service commands cannot implicitly start a project or use a custom entry"
        );
        match action {
            Action::Start => {
                let (entry, status) = manager
                    .start(manager.specification(home, &service_overrides)?)
                    .await?;
                println!(
                    "后台工作台已启动；活动项目：{}。使用 code-view stop 完整退出。",
                    status.active_runs
                );
                registration_guidance(
                    &status,
                    std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
                    args.no_open,
                    || open_entry(&entry, true),
                )
                .await;
            }
            Action::Autostart { mode } => {
                let enabled = matches!(mode, AutoStart::On);
                manager.set_autostart(enabled, manager.specification(home, &service_overrides)?)?;
                println!(
                    "下次登录自启已{}；当前运行状态不变。",
                    if enabled { "开启" } else { "关闭" }
                );
            }
            Action::Status { .. }
            | Action::Stop
            | Action::Open { .. }
            | Action::ServiceMaintenance { .. } => unreachable!(),
        }
        return Ok(());
    }
    if let Some(label) = &args.managed_service {
        manager.verify_invocation(label)?;
    }
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
    if let Some(label) = args.managed_service {
        application.set_service_label(label);
    }
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
            _=health.tick()=>{if application.shutdown_requested(){break;} ensure!(application.healthy(),"application runtime stopped unexpectedly");},
        }
    }
    drop(application);
    Ok(())
}

async fn registration_guidance<F, Fut>(
    status: &management::Status,
    interactive: bool,
    no_open: bool,
    open: F,
) where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    if status.registration_required() {
        eprintln!(
            "电脑尚未注册或需要重新授权。请在公网接入页登录可信中转并批准注册：code-view open --relay"
        );
        if interactive && !no_open && open().await.is_err() {
            eprintln!("浏览器未能打开；请运行 code-view open --relay。");
        }
    } else if status.relay.is_none() {
        eprintln!("公网接入状态暂不可用；请运行 code-view open --relay 检查。");
    }
}

async fn open_entry(
    entry: &codex_local_observer::workbench::launch::BrowserEntry,
    relay: bool,
) -> Result<()> {
    let mut url = reqwest::Url::parse(&entry.url)?;
    if relay {
        url.set_path("/relay");
        url.set_query(None);
    }
    open_browser(url.as_str()).await
}

async fn existing_action(action: Action, manager: &service::Manager, home: PathBuf) -> Result<()> {
    match action {
        Action::Stop => {
            manager.stop().await?;
            println!("工作台及其所属项目已退出。");
        }
        Action::Status { json } => {
            let status = if let Some(entry) = manager.running_entry()? {
                Some(management::control(&entry, management::Action::Status).await?)
            } else {
                None
            };
            let auto = manager.autostart_enabled()?;
            let state = if status.is_some() {
                "running"
            } else if cfg!(target_os = "macos") && manager.loaded().await? {
                "startingOrFailed"
            } else {
                "stopped"
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({"state":state,"autostart":auto,"application":status})
                );
            } else if let Some(s) = status {
                println!(
                    "工作台：{}；活动项目：{}；登录自启：{}；公网连接：{}",
                    if s.service_label.is_some() {
                        "后台运行"
                    } else {
                        "前台运行"
                    },
                    s.active_runs,
                    if auto { "开启" } else { "关闭" },
                    s.relay.as_ref().map_or("不可用", |r| r.state.as_str())
                );
                if s.registration_required() {
                    println!("需要注册或重新授权：code-view open --relay");
                }
            } else {
                println!(
                    "工作台：{}；登录自启：{}",
                    if state == "stopped" {
                        "未运行"
                    } else {
                        "启动中或异常退出，请检查私有 service.log"
                    },
                    if auto { "开启" } else { "关闭" }
                );
            }
        }
        Action::Open { relay, .. } => {
            let entry = manager
                .running_entry()?
                .ok_or_else(|| anyhow::anyhow!("workbench is not running; use code-view start"))?;
            management::control(&entry, management::Action::Status).await?;
            open_entry(&entry, relay).await?;
        }
        Action::Autostart {
            mode: AutoStart::Off,
        } => {
            manager.set_autostart(false, manager.specification(home, &Overrides::default())?)?;
            println!("下次登录自启已关闭；当前运行状态不变。");
        }
        _ => unreachable!(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registration_guidance_respects_saved_intent_and_never_blocks_service_readiness() {
        use std::cell::Cell;
        for (state, registered, enabled, rejected, interactive, no_open, expected) in [
            ("disabled", false, false, false, true, false, 1),
            ("online", true, true, false, true, false, 0),
            ("disabled", true, false, false, true, false, 0),
            ("offline", true, true, false, true, false, 0),
            ("authenticationRequired", true, false, true, true, false, 1),
            ("disabled", false, false, false, false, false, 0),
            ("disabled", false, false, false, true, true, 0),
        ] {
            let status = management::Status {
                instance_id: uuid::Uuid::new_v4(),
                process_id: 1,
                service_label: None,
                stopping: false,
                active_runs: 0,
                relay: Some(management::RelayStatus {
                    state: state.into(),
                    registered,
                    enabled,
                    authentication_required: rejected,
                }),
            };
            let calls = Cell::new(0);
            // A failed browser launcher must remain guidance only. No state
            // mutation, registration wait, or startup failure may result.
            registration_guidance(&status, interactive, no_open, || async {
                calls.set(calls.get() + 1);
                anyhow::bail!("synthetic browser failure")
            })
            .await;
            assert_eq!(
                calls.get(),
                expected,
                "{state}, interactive={interactive}, no_open={no_open}"
            );
            assert_eq!(status.relay.as_ref().unwrap().enabled, enabled);
        }
    }

    #[test]
    fn service_commands_and_explicit_entry_preserve_cli_compatibility() {
        for arguments in [
            vec![
                "code-view",
                "--config-dir",
                "/tmp/config",
                "start",
                "--no-open",
            ],
            vec![
                "code-view",
                "status",
                "--json",
                "--data-dir",
                "/tmp/history",
            ],
            vec!["code-view", "open", "--relay"],
            vec!["code-view", "stop"],
            vec!["code-view", "autostart", "on"],
            vec!["code-view", "autostart", "off"],
        ] {
            assert!(Args::try_parse_from(arguments).unwrap().command.is_some());
        }
        let args = Args::try_parse_from(["code-view", "open", "/tmp/private-entry.json"]).unwrap();
        assert!(matches!(
            args.command,
            Some(Action::Open {
                entry_file: Some(_),
                relay: false
            })
        ));
        let args =
            Args::try_parse_from(["code-view", "--project", "./start", "--no-open"]).unwrap();
        assert!(args.command.is_none());
        assert_eq!(args.project, Some(PathBuf::from("./start")));
        assert!(Args::try_parse_from(["code-view", "autostart", "invalid"]).is_err());
    }

    #[test]
    fn version_reports_the_workbench_binary_and_package_version() {
        let error = Args::try_parse_from(["codex-view", "--version"])
            .err()
            .unwrap();
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
        assert_eq!(
            error.to_string(),
            format!("codex-view {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}
