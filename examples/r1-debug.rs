//! Isolated ordinary CLI + production PTY/proxy/workbench. Synthetic model only.
#[path = "../src/workbench/probe_process.rs"]
mod probe_process;

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Result, ensure};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use clap::Parser;
use codex_local_observer::workbench::{
    capture,
    decode::DecoderLimits,
    live::{LiveHub, LiveLimits},
    observe::Observer,
    proxy::{ProxyServer, Upstream},
    redaction::RedactionPolicy,
    rollout::RolloutReader,
    terminal::TerminalHost,
    web::ReadingServer,
};
use portable_pty::CommandBuilder;
use serde_json::{Value, json};

#[derive(Parser)]
struct Args {
    /// New private file for the browser pairing entry; do not commit it.
    #[arg(long)]
    state_file: PathBuf,
    /// Run the installed Chrome acceptance script and exit after verification.
    #[arg(long)]
    probe: bool,
    /// Optional synthetic-page screenshot from the Chrome probe.
    #[arg(long)]
    screenshot: Option<PathBuf>,
}
fn event(value: Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}
fn model(text: String, id: usize, pace: bool) -> Response {
    let stream = async_stream::stream! {
        let response=format!("resp_r1_{id}"); let item=format!("msg_r1_{id}");
        yield Ok::<_,std::io::Error>(event(json!({"type":"response.created","response":{"id":response,"model":"gpt-6-astra"}})));
        yield Ok(event(json!({"type":"response.output_item.added","output_index":0,"item":{"id":item,"type":"message","role":"assistant","content":[]}})));
        for chunk in text.chars().collect::<Vec<_>>().chunks(12) {
            if pace { tokio::time::sleep(Duration::from_millis(45)).await; }
            yield Ok(event(json!({"type":"response.output_text.delta","item_id":item,"content_index":0,"delta":chunk.iter().collect::<String>()})));
        }
        yield Ok(event(json!({"type":"response.output_item.done","output_index":0,"item":{"id":item,"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}})));
        yield Ok(event(json!({"type":"response.completed","response":{"id":response}})));
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut state_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&args.state_file)?;
    let directory = tempfile::tempdir()?;
    let home = directory.path().join("home");
    let workspace = directory.path().join("native-workbench-demo");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&workspace)?;
    std::fs::write(
        workspace.join("README.md"),
        "# Synthetic workbench demo\nNo real model or project data.\n",
    )?;
    std::fs::write(
        home.join("config.toml"),
        r#"model="gpt-6-astra"
model_provider="custom"
check_for_update_on_startup=false
[model_providers.custom]
name="Local synthetic R1 model"
base_url="http://127.0.0.1:1/original/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-r1-only"
"#,
    )?;
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let upstream = format!("http://{}/v1", listener.local_addr()?);
    let app=Router::new().route("/v1/responses",post(move |request:Request| {
        let count=count.clone();
        async move {
            if request.headers().get(header::AUTHORIZATION).and_then(|v|v.to_str().ok()) != Some("Bearer synthetic-r1-only") {
                return Response::builder().status(StatusCode::FORBIDDEN).body(Body::empty()).unwrap();
            }
            let bytes=axum::body::to_bytes(request.into_body(),2*1024*1024).await.unwrap();
            let value:Value=serde_json::from_slice(&bytes).unwrap();
            if value["synthetic_unclassified"] == true {
                return model("R1_UNKNOWN_REQUEST_CONTENT".into(),0,false);
            }
            if value.pointer("/text/format/schema/properties/title").is_some() {
                return model("{\"title\":\"本机工作台调试\"}".into(),0,false);
            }
            let id=count.fetch_add(1,Ordering::SeqCst)+1;
            let text=format!("R1_NATIVE_BROWSER_OK\n\n这是本机合成模型的流式回复，没有访问真实模型。\n\n{}\n\n**本次模型响应已结束。**\nR1_STREAM_DONE",(1..=14).map(|n|format!("{n}. 原生终端负责输入、审批和设置；中央同步阅读模型正文。切换阅读面板、折叠终端或刷新页面，都应保留同一个 CLI 进程。\n\n")).collect::<String>());
            model(text,id,true)
        }
    }));
    let fixture = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let (capture, receiver) = capture::channel(8 * 1024 * 1024, 256);
    let proxy = ProxyServer::bind(Upstream::parse(&upstream)?, capture.clone()).await?;
    let hub = LiveHub::new(LiveLimits::default());
    let policy = RedactionPolicy::new(vec!["synthetic-r1-only".into()])?;
    let _observer = Observer::start(
        receiver,
        hub.clone(),
        policy.clone(),
        DecoderLimits::default(),
    )?;
    let _users = RolloutReader::start_with_delay(
        &home,
        hub.clone(),
        policy,
        if args.probe {
            Duration::from_secs(4)
        } else {
            Duration::ZERO
        },
    )?;
    let mut command = CommandBuilder::new(
        std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into()),
    );
    command.cwd(&workspace);
    command.env("HOME", directory.path());
    command.env("USERPROFILE", directory.path());
    command.env("CODEX_HOME", &home);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    // Match the product's color terminal and static native presentation.
    command.env_remove("NO_COLOR");
    command.args([
        "-c",
        &format!(
            "model_providers.custom.base_url={}",
            toml::Value::String(proxy.child_base_url())
        ),
    ]);
    command.args(["-c", "tui.animations=false"]);
    let terminal = TerminalHost::spawn(hub.epoch(), command, 45, 120)?;
    let web = ReadingServer::bind_with_terminal(hub, terminal.handle()).await?;
    write!(
        state_file,
        "{}",
        json!({"url":web.bootstrap_url(),"address":format!("http://{}",web.address()),"pid":std::process::id(),"cliPid":terminal.process_id(),"synthetic":true})
    )?;
    state_file.sync_all()?;
    println!(
        "{}",
        json!({"stage":"ready","address":format!("http://{}",web.address()),"pid":std::process::id(),"mode":"R1 native terminal synthetic debug","stateFile":args.state_file})
    );
    if args.probe {
        // A second protocol-valid request has no trustworthy Codex purpose.
        // It must remain in request reading even though its input has role:user.
        let response = reqwest::Client::builder().no_proxy().build()?
            .post(format!("{}/responses", proxy.child_base_url()))
            .header(header::AUTHORIZATION, "Bearer synthetic-r1-only")
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"model":"synthetic-request-model","input":[{"role":"user","content":"synthetic context, not human evidence"}],"synthetic_unclassified":true}).to_string())
            .send().await?;
        ensure!(
            response.status().is_success(),
            "synthetic unclassified request failed"
        );
        let _ = response.bytes().await?;
        let mut command = tokio::process::Command::new("node");
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/web/e2e/r1-terminal-probe.cjs"
            ))
            .env("WORKBENCH_PROBE_URL", web.bootstrap_url())
            .env("DEBUG", "")
            .env("PWDEBUG", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::null());
        if let Some(path) = args.screenshot {
            command.env("WORKBENCH_PROBE_SCREENSHOT", path);
        }
        let mut child = probe_process::ProbeProcess::spawn(&mut command)?;
        // Child::wait closes stdin automatically. Keep this pipe alive until
        // the probe completes; EOF is the probe's parent-disappeared signal.
        let _probe_liveness = child.stdin.take();
        let status = tokio::time::timeout(Duration::from_secs(100), child.wait()).await??;
        ensure!(
            status.success(),
            "Chrome terminal acceptance failed (see sanitized stage)"
        );
        ensure!(
            requests.load(Ordering::SeqCst) == 2,
            "browser actions unexpectedly repeated a model request"
        );
        ensure!(
            capture.stats().dropped_chunks == 0,
            "unexpected observation gaps during browser acceptance"
        );
        println!(
            "{}",
            json!({"stage":"verified","mainRequests":2,"captureDrops":0,"cliPid":terminal.process_id()})
        );
    } else {
        tokio::signal::ctrl_c().await?;
    }
    drop(web);
    drop(terminal);
    fixture.abort();
    std::fs::remove_file(&args.state_file)?;
    Ok(())
}
