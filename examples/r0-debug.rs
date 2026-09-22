//! Persistent local inspection of the implemented R0 path. Synthetic provider,
//! isolated ordinary CLI, production proxy/observer/page. No real model calls.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use clap::Parser;
use codex_local_observer::workbench::capture;
use codex_local_observer::workbench::decode::DecoderLimits;
use codex_local_observer::workbench::live::{LiveHub, LiveLimits};
use codex_local_observer::workbench::observe::Observer;
use codex_local_observer::workbench::proxy::{ProxyServer, Upstream};
use codex_local_observer::workbench::redaction::RedactionPolicy;
use codex_local_observer::workbench::web::ReadingServer;
use serde_json::{Value, json};

#[path = "../src/workbench/test_native.rs"]
mod test_native;
use test_native::NativeProbe;

#[derive(Parser)]
struct Args {
    /// New private file for the local browser pairing entry; never checked in.
    #[arg(long)]
    state_file: PathBuf,
}

const PROMPT: &str = "请输出本地调试阶段说明，不使用工具。";
const TEXT: &str = "【本地合成调试 · 当前可运行阶段】\n\n\
这段文字由本机测试服务按片段发送，没有调用真实模型。普通官方 Codex CLI 正在临时项目与隔离 CODEX_HOME 中运行；请求经过实际 ModelProxy，观察器解码后把正文直接推送到当前网页。\n\n\
你现在可以查看：\n\
1. 回复尚未结束时，网页逐步显示中间正文。\n\
2. 上滚阅读时暂停跟随，点击“跟随最新”回到末尾。\n\
3. 刷新后恢复本次运行已捕获的内容，刷新不会重新发送模型请求。\n\
4. 底部明确显示“保存未启用”；捕获详情会解释当前省略的内容。\n\n\
当前尚未接通：三列工作台中的 xterm 终端、文件与 Git 面板、工具结果和磁盘历史。方案里的三列页面仍是交互原型。终端单写和接管规则已有单元测试，浏览器输入体验仍在 R1 开发中。\n\n\
这次调试保持页面和 CLI 存活，便于你检查。结束调试进程后，临时项目和临时配置会清理；当前页面的内存阅读记录不作持久保存。\n\n\
R0_DEBUG_COMPLETE：本次合成模型响应发送结束。";

fn event(value: Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}

fn response_stream(text: String, paced: bool, id: usize) -> Response {
    let stream = async_stream::stream! {
        let response_id=format!("resp_debug_{id}"); let item_id=format!("msg_debug_{id}");
        yield Ok::<_,std::io::Error>(event(json!({"type":"response.created","response":{"id":response_id}})));
        yield Ok(event(json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant","id":item_id,"content":[]}})));
        for chunk in text.chars().collect::<Vec<_>>().chunks(9) {
            if paced { tokio::time::sleep(Duration::from_millis(260)).await; }
            yield Ok(event(json!({"type":"response.output_text.delta","item_id":item_id,"content_index":0,"delta":chunk.iter().collect::<String>()})));
        }
        yield Ok(event(json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","id":item_id,"content":[{"type":"output_text","text":text}]}})));
        yield Ok(event(json!({"type":"response.completed","response":{"id":response_id}})));
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let args = Args::parse();
    // Fail before creating runtime resources if the requested state file exists.
    let mut state_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&args.state_file)?;
    let directory = tempfile::tempdir()?;
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&workspace)?;
    std::fs::write(
        workspace.join("README.md"),
        "# Synthetic local R0 debug\nNo real project files or credentials.\n",
    )?;
    std::fs::write(
        home.join("config.toml"),
        r#"model="gpt-6-astra"
model_provider="custom"
check_for_update_on_startup=false
[model_providers.custom]
name="Local synthetic debug provider"
base_url="http://127.0.0.1:1/original/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-debug-only"
"#,
    )?;
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let upstream = format!("http://{}/v1", listener.local_addr()?);
    let fixture = Router::new().route(
        "/v1/responses",
        post(move |request: Request| {
            let count = count.clone();
            async move {
                if request.headers().contains_key(header::ORIGIN)
                    || request
                        .headers()
                        .get(header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        != Some("Bearer synthetic-debug-only")
                {
                    return Response::builder()
                        .status(StatusCode::FORBIDDEN)
                        .body(Body::empty())
                        .unwrap();
                }
                let Ok(bytes) = axum::body::to_bytes(request.into_body(), 2 * 1024 * 1024).await
                else {
                    return Response::builder()
                        .status(StatusCode::PAYLOAD_TOO_LARGE)
                        .body(Body::empty())
                        .unwrap();
                };
                let Ok(body) = serde_json::from_slice::<Value>(&bytes) else {
                    return Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Body::empty())
                        .unwrap();
                };
                if body
                    .pointer("/text/format/schema/properties/title")
                    .is_some()
                {
                    return response_stream("{\"title\":\"本地合成调试\"}".into(), false, 0);
                }
                let id = count.fetch_add(1, Ordering::SeqCst) + 1;
                response_stream(TEXT.into(), true, id)
            }
        }),
    );
    let fixture_task = tokio::spawn(async move {
        let _ = axum::serve(listener, fixture).await;
    });
    let (capture, receiver) = capture::channel(8 * 1024 * 1024, 256);
    let proxy = ProxyServer::bind(Upstream::parse(&upstream)?, capture.clone()).await?;
    let hub = LiveHub::new(LiveLimits::default());
    let _observer = Observer::start(
        receiver,
        hub.clone(),
        RedactionPolicy::new(vec!["synthetic-debug-only".into()])?,
        DecoderLimits::default(),
    )?;
    let web = ReadingServer::bind(hub.clone()).await?;
    write!(
        state_file,
        "{}",
        json!({"url":web.bootstrap_url(),"address":format!("http://{}",web.address()),"pid":std::process::id(),"runEpoch":hub.epoch(),"synthetic":true})
    )?;
    state_file.sync_all()?;
    println!(
        "{}",
        json!({"stage":"ready","address":format!("http://{}",web.address()),"pid":std::process::id(),"mode":"R0 synthetic debug","stateFile":args.state_file})
    );
    let mut native = NativeProbe::start(
        &home,
        &workspace,
        &[
            "-c".into(),
            format!(
                "model_providers.custom.base_url={}",
                toml::Value::String(proxy.child_base_url())
            ),
        ],
    )?;
    native.ready().await?;
    // Give the browser a short opportunity to pair before the visible stream.
    let ready = Instant::now();
    while ready.elapsed() < Duration::from_secs(3) {
        native.pump().await?;
    }
    native.submit(PROMPT).await?;
    println!(
        "{}",
        json!({"stage":"native-input-submitted","synthetic":true})
    );
    let mut completed = false;
    loop {
        tokio::select! {
            _=tokio::signal::ctrl_c()=>break,
            result=native.pump()=>{ result?; }
        }
        if !completed
            && native.screen().contains("R0_DEBUG_COMPLETE")
            && hub
                .snapshot()
                .model_items()
                .iter()
                .any(|item| item.text == TEXT)
        {
            ensure!(
                requests.load(Ordering::SeqCst) == 1,
                "debug unexpectedly repeated a model turn"
            );
            completed = true;
            println!(
                "{}",
                json!({"stage":"response-complete","mainRequests":1,"keptAlive":true,"captureDrops":capture.stats().dropped_chunks})
            );
        }
    }
    let _ = native.quit().await;
    drop(native);
    fixture_task.abort();
    std::fs::remove_file(args.state_file)?;
    println!("Local debug stopped; temporary workspace removed.");
    Ok(())
}
