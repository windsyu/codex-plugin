//! Synthetic, isolated official CLI protocol spike; never a production launcher.
//! Run: cargo run --locked --example v3-native-channel-probe
use anyhow::{Context, Result, bail, ensure};
use axum::{
    Router,
    body::{Body, Bytes},
    response::Response,
    routing::post,
};
use clap::Parser;
use codex_local_observer::workbench::{
    capture,
    channel::{Evidence, NativeStatus, OriginProof},
    proxy::{ProxyServer, Upstream},
    terminal::{TerminalEvent, TerminalHost},
};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    /// New safe aggregate report file; never overwritten.
    #[arg(long)]
    report: Option<PathBuf>,
}
use futures_util::{SinkExt, StreamExt};
use portable_pty::CommandBuilder;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::net::UnixStream;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};
use uuid::Uuid;

struct Rpc {
    socket: WebSocketStream<UnixStream>,
    next: u64,
    events: VecDeque<Value>,
}
impl Rpc {
    async fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path).await?;
        let (socket, _) = tokio_tungstenite::client_async("ws://localhost/rpc", stream).await?;
        let mut rpc = Self {
            socket,
            next: 0,
            events: VecDeque::new(),
        };
        rpc.call("initialize", json!({"clientInfo":{"name":"v3_synthetic_probe","version":"1"},"capabilities":{"experimentalApi":true}})).await?;
        rpc.socket
            .send(Message::Text(
                json!({"method":"initialized"}).to_string().into(),
            ))
            .await?;
        Ok(rpc)
    }
    async fn read(&mut self) -> Result<Value> {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(25), self.socket.next())
                .await
                .context("protocol deadline")?
                .context("protocol closed")??;
            if let Message::Text(text) = message {
                return Ok(serde_json::from_str(&text)?);
            }
        }
    }
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        self.socket
            .send(Message::Text(
                json!({"id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await?;
        loop {
            let value = self.read().await?;
            if value["id"] == id && value.get("method").is_none() {
                ensure!(
                    value.get("error").is_none(),
                    "RPC {method} failed (payload suppressed)"
                );
                return Ok(value["result"].clone());
            }
            self.events.push_back(value);
        }
    }
    async fn event(&mut self) -> Result<Value> {
        match self.events.pop_front() {
            Some(value) => Ok(value),
            None => self.read().await,
        }
    }
}
fn event(value: Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}
fn response(index: usize, tool: bool) -> Response {
    let stream = async_stream::stream! {
        let id = format!("resp_probe_{index}");
        yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":id,"model":"gpt-6-astra"}})));
        if !tool { tokio::time::sleep(Duration::from_millis(300)).await; }
        let item = if tool { json!({"id":format!("call_{index}"),"type":"function_call","call_id":format!("call_{index}"),"name":"update_plan","arguments":"{\"plan\":[{\"step\":\"Synthetic no-op\",\"status\":\"completed\"}]}"}) } else { json!({"id":format!("msg_{index}"),"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"SYNTHETIC_REPLY"}]}) };
        yield Ok(event(json!({"type":"response.output_item.added","output_index":0,"item":item})));
        yield Ok(event(json!({"type":"response.output_item.done","output_index":0,"item":item})));
        yield Ok(event(json!({"type":"response.completed","response":{"id":id}})));
    };
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}
fn config(home: &Path, workspace: &Path, url: &str) -> Result<()> {
    std::fs::create_dir_all(home)?;
    std::fs::write(
        home.join("config.toml"),
        format!(
            r#"model="gpt-6-astra"
model_provider="synthetic"
check_for_update_on_startup=false
[features]
daemon_auto_start=false
[model_providers.synthetic]
name="Isolated synthetic probe"
base_url="{url}"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-probe-only"
[projects.{}]
trust_level="trusted"
"#,
            toml::Value::String(workspace.display().to_string())
        ),
    )?;
    std::fs::create_dir_all(home.join("app-server-daemon"))?;
    std::fs::write(
        home.join("app-server-daemon/settings.json"),
        r#"{"remoteControlEnabled":false,"updater":{"autoUpdateEnabled":false}}"#,
    )?;
    Ok(())
}
fn tui(
    binary: &std::ffi::OsStr,
    home: &Path,
    workspace: &Path,
    args: &[String],
) -> Result<TerminalHost> {
    let mut command = CommandBuilder::new(binary);
    command.env_clear();
    command.env("PATH", std::env::var_os("PATH").unwrap_or_default());
    command.env("HOME", home);
    command.env("USERPROFILE", home);
    command.env("CODEX_HOME", home);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.cwd(workspace);
    command.args(args);
    TerminalHost::spawn(Uuid::new_v4(), command, 45, 120)
}
// ANSI is used solely for initializing the synthetic TUI and placing one local
// test input. It never proves submission, channel attribution, or completion.
async fn initialize_tui(host: &TerminalHost, prompt: &str) -> Result<()> {
    let handle = host.handle();
    let mut attachment = handle.attach().await?;
    let grant = handle.claim(attachment.connection_id).await?;
    let mut parser = vt100::Parser::new(45, 120, 0);
    parser.process(&attachment.snapshot.screen);
    parser.process(&attachment.snapshot.replay);
    let mut sequence = 0;
    let mut theme = false;
    let mut notice = false;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let screen = parser.screen().contents();
            let bytes = if !theme
                && (screen.contains("Choose your style") || screen.contains("Select a theme"))
            {
                theme = true;
                Some(b"\r".to_vec())
            } else if !notice
                && screen.contains("Try new model")
                && screen.contains("Use existing model")
            {
                notice = true;
                Some(b"\x1b[B\r".to_vec())
            } else if screen.contains("OpenAI Codex")
                && screen
                    .lines()
                    .any(|line| line.trim_start().starts_with('›'))
            {
                break;
            } else {
                None
            };
            if let Some(bytes) = bytes {
                sequence += 1;
                handle
                    .input(attachment.connection_id, grant.generation, sequence, bytes)
                    .await?;
            }
            if let Some(TerminalEvent::Output(output)) = attachment.events.recv().await {
                parser.process(&output.data);
            }
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
        sequence += 1;
        handle
            .input(
                attachment.connection_id,
                grant.generation,
                sequence,
                format!("\x1b[200~{prompt}\x1b[201~").into_bytes(),
            )
            .await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        sequence += 1;
        handle
            .input(
                attachment.connection_id,
                grant.generation,
                sequence,
                b"\r".to_vec(),
            )
            .await?;
        let until = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut trusted = false;
        while tokio::time::Instant::now() < until {
            if let Ok(Some(TerminalEvent::Output(output))) = tokio::time::timeout(Duration::from_millis(100), attachment.events.recv()).await { parser.process(&output.data); }
            let screen = parser.screen().contents();
            if !trusted && screen.contains("Trust this folder?") { trusted=true;sequence+=1;handle.input(attachment.connection_id,grant.generation,sequence,b"\r".to_vec()).await?; }
        }
        let screen = parser.screen().contents();
        println!("{}",json!({"check":"synthetic-tui-driver","trustedPromptHandled":trusted,"error":screen.to_lowercase().contains("error"),"retry":screen.contains("Retry"),"syntheticDraftVisible":screen.contains(prompt),"folderAccess":screen.contains("Folder access"),"modelNotice":screen.contains("Use existing model")}));
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("synthetic TUI initialization deadline")??;
    handle
        .release(attachment.connection_id, grant.generation)
        .await?;
    Ok(())
}
// Only native semantic lifecycle envelopes feed the production gate.
fn native_evidence(event: &Value) -> Result<Option<Evidence<'_>>> {
    let params = &event["params"];
    let required = |value: &Value| -> Result<()> {
        ensure!(
            value.is_string(),
            "protocol-not-supported: missing native identity"
        );
        Ok(())
    };
    let method = event["method"].as_str().unwrap_or("");
    let item = &params["item"];
    let is_user = item["type"] == "userMessage";
    let is_final = method == "item/completed" && item["type"] == "agentMessage";
    if !(matches!(method, "turn/started" | "turn/completed")
        || (matches!(method, "item/started" | "item/completed") && is_user)
        || is_final)
    {
        return Ok(None);
    }
    required(&params["threadId"])?;
    let thread = params["threadId"].as_str().unwrap();
    if matches!(method, "turn/started" | "turn/completed") {
        required(&params["turn"]["id"])?;
        let turn = params["turn"]["id"].as_str().unwrap();
        return Ok(Some(if method == "turn/started" {
            Evidence::TurnStarted { thread, turn }
        } else {
            Evidence::TurnCompleted {
                thread,
                turn,
                status: match params["turn"]["status"].as_str() {
                    Some("completed") => NativeStatus::Completed,
                    Some("failed") => NativeStatus::Failed,
                    Some("interrupted") => NativeStatus::Interrupted,
                    _ => NativeStatus::Unknown,
                },
            }
        }));
    }
    required(&params["turnId"])?;
    required(&item["id"])?;
    let turn = params["turnId"].as_str().unwrap();
    let id = item["id"].as_str().unwrap();
    if is_user {
        let client = item["clientId"].as_str();
        return Ok(Some(if method == "item/started" {
            Evidence::UserStarted {
                thread,
                turn,
                item: id,
                client,
            }
        } else {
            Evidence::UserCompleted {
                thread,
                turn,
                item: id,
                client,
            }
        }));
    }
    match item["phase"].as_str() {
        Some("commentary") => Ok(None),
        Some("final_answer") => {
            required(&item["text"])?;
            Ok(Some(Evidence::FinalAnswer {
                thread,
                turn,
                item: id,
                text: item["text"].as_str().unwrap(),
            }))
        }
        _ => bail!("protocol-not-supported: native agent message has no supported explicit phase"),
    }
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let binary = std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into());
    // Keep UDS under its path limit even on macOS's long default TMPDIR.
    let directory = tempfile::Builder::new()
        .prefix("v3probe-")
        .tempdir_in("/tmp")?;
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let workspace = workspace.canonicalize()?;
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let per_turn = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
    let observed = per_turn.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/v1", listener.local_addr()?);
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/responses",
                post(move |request: axum::extract::Request| {
                    let seen = seen.clone();
                    let observed = observed.clone();
                    async move {
                        let headers = request.headers().clone();
                        let bytes = axum::body::to_bytes(request.into_body(), 16 * 1024 * 1024)
                            .await
                            .unwrap();
                        let body: Value = serde_json::from_slice(&bytes).unwrap();
                        let metadata = body["client_metadata"]["x-codex-turn-metadata"]
                            .as_str()
                            .or_else(|| {
                                headers
                                    .get("x-codex-turn-metadata")
                                    .and_then(|v| v.to_str().ok())
                            })
                            .unwrap_or("{}");
                        let metadata: Value = serde_json::from_str(metadata).unwrap();
                        let turn = metadata["turn_id"].as_str().unwrap_or("missing").to_owned();
                        let n = {
                            let mut counts = observed.lock().unwrap();
                            let count = counts.entry(turn).or_default();
                            *count += 1;
                            *count
                        };
                        response(seen.fetch_add(1, Ordering::SeqCst), n == 1)
                    }
                }),
            ),
        )
        .await
    });
    let (capture, receiver) = capture::channel(1024 * 1024, 32);
    drop(receiver); // This spike exercises forwarding independently of observation.
    let proxy = ProxyServer::bind(Upstream::parse(&url)?, capture).await?;
    let url = proxy.child_base_url();
    config(&home, &workspace, &url)?;
    let version = tokio::process::Command::new(&binary)
        .arg("--version")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("CODEX_HOME", &home)
        .output()
        .await?;
    ensure!(version.status.success(), "CLI version failed");
    println!(
        "{}",
        json!({"check":"installed-cli","version":String::from_utf8_lossy(&version.stdout).trim()})
    );
    let current = tui(
        &binary,
        &home,
        &workspace,
        &[
            "-c".into(),
            format!(
                "model_providers.synthetic.base_url={}",
                toml::Value::String(url.clone())
            ),
            "-c".into(),
            "tui.animations=false".into(),
        ],
    )?;
    initialize_tui(&current, "SYNTHETIC_LOCAL").await?;
    tokio::time::timeout(Duration::from_secs(20), async {
        while count.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("ordinary CLI reached no synthetic model")?;
    let sockets = walkdir::WalkDir::new(&home)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            use std::os::unix::fs::FileTypeExt;
            e.file_type().is_socket()
        })
        .count();
    ensure!(
        sockets == 0,
        "ordinary overridden CLI unexpectedly exposed a socket; reassess route"
    );
    println!(
        "{}",
        json!({"check":"current-ordinary-overridden-route","modelReached":true,"socketsUnderIsolatedHome":sockets,"attachEndpointAvailable":false})
    );
    current.handle().stop().await?;
    drop(current);
    let candidate_home = directory.path().join("candidate");
    config(&candidate_home, &workspace, &url)?;
    let socket = directory.path().join("rpc.sock");
    let mut child = tokio::process::Command::new(&binary)
        .args(["app-server", "--listen"])
        .arg(format!("unix://{}", socket.display()))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &candidate_home)
        .env("USERPROFILE", &candidate_home)
        .env("CODEX_HOME", &candidate_home)
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    tokio::time::timeout(Duration::from_secs(15), async {
        while !socket.exists() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("private app-server socket deadline")?;
    let mut rpc = Rpc::connect(&socket).await?;
    let native = tui(
        &binary,
        &candidate_home,
        &workspace,
        &["--remote".into(), format!("unix://{}", socket.display())],
    )?;
    initialize_tui(&native, "SYNTHETIC_LOCAL").await?;
    let thread = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let loaded = rpc.call("thread/loaded/list", json!({})).await?;
            if let Some(id) = loaded["data"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(Value::as_str)
            {
                break Ok::<_, anyhow::Error>(id.to_owned());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    rpc.call("thread/resume", json!({"threadId":thread}))
        .await?;
    // Read structured history, never terminal text, to establish the local input.
    let local = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let history = rpc
                .call(
                    "thread/read",
                    json!({"threadId":thread,"includeTurns":true}),
                )
                .await?;
            let turns = history["thread"]["turns"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if turns.iter().any(|t| t["status"] == "completed") {
                break Ok::<_, anyhow::Error>(turns);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    ensure!(
        local
            .iter()
            .flat_map(|t| t["items"].as_array().into_iter().flatten())
            .any(|i| i["type"] == "userMessage"),
        "missing structured local input"
    );
    println!(
        "{}",
        json!({"check":"native-tui-local-turn","sameLoadedThread":true,"channelAttributed":false})
    );
    let local_ids: Vec<String> = local
        .iter()
        .flat_map(|t| t["items"].as_array().into_iter().flatten())
        .filter(|i| i["type"] == "userMessage")
        .filter_map(|i| i["clientId"].as_str().map(str::to_owned))
        .collect();
    let mut completed_turns = Vec::new();
    for _ in 0..2 {
        let client_id = Uuid::new_v4();
        let client = client_id.to_string();
        ensure!(
            !local_ids.contains(&client),
            "local input incorrectly channel-attributed"
        );
        // Register before the queue RPC: notifications may precede its response.
        let mut proof = OriginProof::new(&thread, client_id).context("invalid probe thread")?;
        let queued=rpc.call("thread/queue/add",json!({"threadId":thread,"input":[{"type":"text","text":"SYNTHETIC_SAME_TEXT"}],"clientUserMessageId":client})).await?;
        ensure!(
            queued["queuedSubmission"]["clientUserMessageId"] == client,
            "queue acknowledgment identity mismatch"
        );
        loop {
            let event = rpc.event().await?;
            if event.get("id").is_some() && event.get("method").is_some() {
                bail!("unexpected interaction; no automatic approval");
            }
            let Some(evidence) = native_evidence(&event)? else {
                continue;
            };
            let terminal = matches!(evidence, Evidence::TurnCompleted { .. });
            let completion = proof.observe(evidence);
            ensure!(
                terminal || completion.is_none(),
                "gate released before native turn completion"
            );
            if let Some(completion) = completion {
                ensure!(
                    completion.status == NativeStatus::Completed && completion.withheld.is_none(),
                    "origin gate withheld native completion"
                );
                ensure!(
                    completion.reply.as_deref() == Some("SYNTHETIC_REPLY"),
                    "protocol-not-supported: explicit final answer evidence missing"
                );
                ensure!(proof.invalidated().is_none(), "origin gate was invalidated");
                completed_turns.push(
                    event["params"]["turn"]["id"]
                        .as_str()
                        .context("missing completed turn")?
                        .to_owned(),
                );
                break;
            }
        }
    }
    ensure!(
        completed_turns[0] != completed_turns[1],
        "same-text tasks unexpectedly merged"
    );
    ensure!(
        completed_turns
            .iter()
            .all(|turn| per_turn.lock().unwrap().get(turn) == Some(&2)),
        "each channel turn must span two model completions"
    );
    println!(
        "{}",
        json!({"check":"native-channel-source-chain","sameTextDistinctClientIds":2,"distinctCompletedTurns":2,"tuiSameLoadedThread":true,"completionEvidence":"turn/completed","originProofPassed":true,"candidateOnly":true,"multiCallCompletion":"two-model-responses-per-turn","approval":"not-tested"})
    );
    native.handle().stop().await?;
    drop(native);
    child.kill().await?;
    child.wait().await?;
    server.abort();
    if let Some(path) = args.report {
        let checks = [
            ("normalProxyForwarded", "Ordinary CLI reached the synthetic upstream through the production model proxy."),
            ("noAttachEndpoint", "No socket existed under the isolated ordinary CLI home with the current command-line overrides."),
            ("sameExistingThread", "A second protocol client attached to the already loaded thread created by the official TUI."),
            ("localInputExcluded", "The structured local user message was excluded from the channel client-ID mapping."),
            ("sameTextDistinctClientIds", "Two identical texts with distinct client IDs produced distinct attributed native turns."),
            ("nativeTurnCompleted", "Actual ordered native events passed OriginProof; only turn/completed released the explicit final-answer item, without withholding."),
            ("twoModelResponsesPerTurn", "Each attributed turn performed a synthetic update_plan call and a second model response before native completion.")
        ].into_iter().map(|(name, detail)|json!({"name":name,"passed":true,"detail":detail})).collect::<Vec<_>>();
        let report = json!({
            "schemaVersion":1,
            "cliVersion":String::from_utf8_lossy(&version.stdout).trim(),
            "sourceCommit":"2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4",
            "checks":checks,
            "notTested":["Approval and user-question round trips", "Interleaved local/channel steering", "Lost acknowledgments and stale operations", "Disconnect and crash recovery", "Installed binary correspondence to the research source commit"]
        });
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .context("create new aggregate report")?;
        file.write_all(&serde_json::to_vec_pretty(&report)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        println!(
            "{}",
            json!({"check":"safe-aggregate-report","written":true})
        );
    }
    Ok(())
}
