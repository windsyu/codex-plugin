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
    channel::{Completion, Evidence, NativeStatus, OriginProof, Withheld},
    proxy::{ProxyServer, Upstream},
    terminal::{TerminalEvent, TerminalHost},
};
#[path = "v3_probe/interaction.rs"]
mod interaction;

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    /// Candidate-only concurrent approval/input consumption probe.
    #[arg(long, conflicts_with_all = ["report", "boundaries", "attachment_boundaries"])]
    interaction_races: bool,
    /// New aggregate file; includes partial results if the probe fails.
    #[arg(long, requires = "interaction_races")]
    interaction_report: Option<PathBuf>,
    /// New safe aggregate report file; never overwritten.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Exercise additional native interaction boundaries; no real model or channel.
    #[arg(long, conflicts_with = "report")]
    boundaries: bool,
    /// New safe aggregate for boundary checks (separate from the P0a report).
    #[arg(long, requires = "boundaries")]
    boundaries_report: Option<PathBuf>,
    /// Tee the TUI subscription; the control client never resumes or queues.
    #[arg(long, conflicts_with_all = ["boundaries", "report"])]
    attachment_boundaries: bool,
    #[arg(long, requires = "attachment_boundaries")]
    attachment_report: Option<PathBuf>,
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
        let envelope = self.call_envelope(method, params).await?;
        ensure!(
            envelope.get("error").is_none(),
            "RPC {method} failed (payload suppressed)"
        );
        Ok(envelope["result"].clone())
    }
    async fn call_envelope(&mut self, method: &str, params: Value) -> Result<Value> {
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
                return Ok(value);
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
    let temporary = home.join("tmp");
    std::fs::create_dir_all(&temporary)?;
    command.env("TMPDIR", &temporary);
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
    if args.interaction_races {
        return interaction::run(args.interaction_report.as_deref()).await;
    }
    let binary = std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into());
    // Keep UDS under its path limit even on macOS's long default TMPDIR.
    let directory = tempfile::Builder::new()
        .prefix("v3probe-")
        .tempdir_in("/tmp")?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let workspace = workspace.canonicalize()?;
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let per_turn = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
    let observed = per_turn.clone();
    let boundary_model = Arc::new(BoundaryModel::default());
    let model_control = boundary_model.clone();
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
                    let model_control = model_control.clone();
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
                        let index = seen.fetch_add(1, Ordering::SeqCst);
                        let mode = model_control.mode.load(Ordering::SeqCst);
                        if mode == 0 {
                            response(index, n == 1)
                        } else {
                            boundary_response(index, n, mode, model_control)
                        }
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
    if args.boundaries || args.attachment_boundaries {
        let path = candidate_home.join("config.toml");
        let prior = std::fs::read_to_string(&path)?;
        std::fs::write(
            path,
            format!(
                "thread_unload_delay_secs=0\napproval_policy=\"on-request\"\napprovals_reviewer=\"user\"\nsandbox_mode=\"workspace-write\"\n{prior}"
            ),
        )?;
    }
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
    let (native_relay, mut tee) = if args.attachment_boundaries {
        let (relay, receiver) = codex_local_observer::workbench::channel::native::Relay::bind(
            directory.path().canonicalize()?.join("tui.sock"),
            socket.canonicalize()?,
            Default::default(),
        )
        .await?;
        (Some(relay), Some(receiver))
    } else {
        (None, None)
    };
    let native_socket = native_relay
        .as_ref()
        .map_or(socket.as_path(), |relay| relay.path());
    let native = tui(
        &binary,
        &candidate_home,
        &workspace,
        &[
            "--remote".into(),
            format!("unix://{}", native_socket.display()),
        ],
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
    if !args.attachment_boundaries {
        rpc.call("thread/resume", json!({"threadId":thread}))
            .await?;
    }
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
    if args.attachment_boundaries {
        let result = tokio::time::timeout(
            Duration::from_secs(90),
            run_attachment_boundaries(
                &mut rpc,
                tee.take().unwrap(),
                &native,
                &thread,
                &boundary_model,
            ),
        )
        .await;
        let _ = native.handle().stop().await;
        drop(native);
        drop(native_relay);
        child.kill().await?;
        child.wait().await?;
        server.abort();
        let checks = result.context("attachment suite deadline")??;
        let report = json!({"schemaVersion":1,"kind":"native-attachment-boundaries","cliVersion":String::from_utf8_lossy(&version.stdout).trim(),"sourceCommit":"2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4","checks":checks,"notTested":["Production launch integration and authorization","Atomic loaded-turn precondition under simultaneous local input","Real Telegram delivery","Installed binary correspondence to research source commit"]});
        if let Some(path) = args.attachment_report {
            write_report(&path, &report)?;
        }
        println!("{}", report);
        return Ok(());
    }
    if args.boundaries {
        let result = tokio::time::timeout(
            Duration::from_secs(160),
            run_boundaries(&mut rpc, &socket, &native, &thread, &boundary_model),
        )
        .await;
        native.handle().stop().await?;
        drop(native);
        child.kill().await?;
        child.wait().await?;
        server.abort();
        let checks = result.context("boundary suite deadline")??;
        let report = json!({"schemaVersion":1,"kind":"native-semantic-boundaries","cliVersion":String::from_utf8_lossy(&version.stdout).trim(),"sourceCommit":"2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4","checks":checks,"notTested":["Actual Telegram delivery and authorization","Native picker preservation","Process crash recovery","Installed binary correspondence to the research source commit"]});
        if let Some(path) = args.boundaries_report {
            write_report(&path, &report)?;
        }
        println!("{}", report);
        return Ok(());
    }
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

#[derive(Default)]
struct BoundaryModel {
    // 0 baseline, 1 approval, 2 question, 3 held model, 4 held after final item.
    mode: AtomicUsize,
    release: tokio::sync::Notify,
    entered: AtomicUsize,
}
fn boundary_response(
    index: usize,
    call: usize,
    mode: usize,
    control: Arc<BoundaryModel>,
) -> Response {
    let stream = async_stream::stream! {
        let response_id=format!("boundary_response_{index}");
        yield Ok::<_,std::io::Error>(event(json!({"type":"response.created","response":{"id":response_id,"model":"gpt-6-astra"}})));
        if mode==3 && call==1 {
            control.entered.fetch_add(1,Ordering::SeqCst);
            let _ = tokio::time::timeout(Duration::from_secs(15),control.release.notified()).await;
        }
        let tool = if call==1 && mode==1 {Some(("exec_command",json!({"cmd":"printf SYNTHETIC","login":false,"sandbox_permissions":"require_escalated","justification":"Isolated synthetic literal-output probe","yield_time_ms":1000})))}
            else if call==1 && mode==2 {Some(("request_user_input",json!({"questions":[{"id":"probe_choice","header":"Probe","question":"Choose the synthetic test option","options":[{"label":"Yes","description":"Synthetic yes"},{"label":"No","description":"Synthetic no"}]}]})))} else {None};
        let item=if let Some((name,arguments))=tool {json!({"type":"function_call","id":format!("boundary_call_{index}"),"call_id":format!("boundary_call_{index}"),"name":name,"arguments":arguments.to_string()})}
            else {json!({"id":format!("boundary_message_{index}"),"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"SYNTHETIC_REPLY"}]})};
        yield Ok(event(json!({"type":"response.output_item.added","output_index":0,"item":item})));
        yield Ok(event(json!({"type":"response.output_item.done","output_index":0,"item":item})));
        if mode==4 && call==1 {
            control.entered.fetch_add(1,Ordering::SeqCst);
            let _ = tokio::time::timeout(Duration::from_secs(15),control.release.notified()).await;
        }
        yield Ok(event(json!({"type":"response.completed","response":{"id":response_id}})));
    };
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}
fn write_report(path: &Path, report: &Value) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .context("create safe boundary report")?;
    file.write_all(&serde_json::to_vec_pretty(report)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
async fn collect_until(
    rpc: &mut Rpc,
    events: &mut Vec<Value>,
    predicate: impl Fn(&Value) -> bool,
) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            ensure!(
                events.len() < 2048,
                "bounded native event capture exhausted"
            );
            let e = rpc.event().await?;
            let matched = predicate(&e);
            events.push(e.clone());
            if matched {
                return Ok(e);
            }
        }
    })
    .await
    .context("boundary semantic event deadline")?
}
async fn respond(rpc: &mut Rpc, id: &Value, result: Value) -> Result<()> {
    rpc.socket
        .send(Message::Text(
            json!({"id":id,"result":result}).to_string().into(),
        ))
        .await?;
    Ok(())
}
async fn barrier(rpc: &mut Rpc, thread: &str) -> Result<()> {
    rpc.call(
        "thread/read",
        json!({"threadId":thread,"includeTurns":false}),
    )
    .await?;
    rpc.events.clear();
    Ok(())
}
async fn start_case(
    rpc: &mut Rpc,
    thread: &str,
    client: Uuid,
    plan: bool,
) -> Result<(String, OriginProof)> {
    let proof = OriginProof::new(thread, client).context("invalid synthetic thread")?;
    let mut params = json!({"threadId":thread,"input":[{"type":"text","text":"SYNTHETIC_BOUNDARY"}],"clientUserMessageId":client.to_string(),"approvalPolicy":"on-request","approvalsReviewer":"user"});
    params["collaborationMode"] = json!({"mode":if plan {"plan"} else {"default"},"settings":{"model":"gpt-6-astra","reasoning_effort":"medium","developer_instructions":null}});
    let result = rpc.call("turn/start", params).await?;
    Ok((
        result["turn"]["id"]
            .as_str()
            .context("missing started turn")?
            .to_owned(),
        proof,
    ))
}
fn replay_proof(mut proof: OriginProof, events: &[Value]) -> Result<Completion> {
    let mut finished = None;
    for e in events {
        if let Some(evidence) = native_evidence(e)? {
            let terminal = matches!(evidence, Evidence::TurnCompleted { .. });
            if let Some(completion) = proof.observe(evidence) {
                ensure!(terminal, "premature gate release");
                ensure!(finished.is_none(), "duplicate gate completion");
                finished = Some(completion);
            }
        }
    }
    finished.context("missing channel gate completion")
}
fn passed(checks: &mut Vec<Value>, name: &str, detail: &str) {
    let value = json!({"name":name,"status":"passed","detail":detail});
    println!("{}", value);
    checks.push(value);
}
async fn completed(rpc: &mut Rpc, events: &mut Vec<Value>, turn: &str) -> Result<()> {
    let e = collect_until(rpc, events, |e| {
        e["method"] == "turn/completed" && e["params"]["turn"]["id"] == turn
    })
    .await?;
    ensure!(
        e["params"]["turn"]["status"] == "completed",
        "native boundary turn did not complete"
    );
    Ok(())
}
async fn wait_model(control: &BoundaryModel, before: usize) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        while control.entered.load(Ordering::SeqCst) == before {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("held synthetic model deadline")?;
    Ok(())
}
async fn run_boundaries(
    rpc: &mut Rpc,
    socket: &Path,
    native: &TerminalHost,
    thread: &str,
    control: &Arc<BoundaryModel>,
) -> Result<Vec<Value>> {
    let mut checks = Vec::new();
    let mut second = Rpc::connect(socket).await?;
    second
        .call("thread/resume", json!({"threadId":thread}))
        .await?;
    for allow in [true, false] {
        barrier(rpc, thread).await?;
        barrier(&mut second, thread).await?;
        control.mode.store(1, Ordering::SeqCst);
        let (turn, proof) = start_case(rpc, thread, Uuid::new_v4(), false).await?;
        let mut events = Vec::new();
        let approval = collect_until(rpc, &mut events, |e| {
            e["method"] == "item/commandExecution/requestApproval" && e["params"]["turnId"] == turn
        })
        .await
        .context("command approval not observed")?;
        ensure!(
            approval["params"]["threadId"] == thread,
            "wrong approval thread"
        );
        let mut other = Vec::new();
        let mirror = collect_until(&mut second, &mut other, |e| {
            e["method"] == "item/commandExecution/requestApproval" && e["params"]["turnId"] == turn
        })
        .await?;
        ensure!(
            mirror["id"] == approval["id"],
            "subscribers received different callbacks"
        );
        respond(
            rpc,
            &approval["id"],
            json!({"decision":if allow {"accept"} else {"decline"}}),
        )
        .await?;
        collect_until(&mut second, &mut other, |e| {
            e["method"] == "serverRequest/resolved" && e["params"]["requestId"] == approval["id"]
        })
        .await?;
        // Send the opposite answer only after resolution: it must not reopen or reverse it.
        respond(
            &mut second,
            &approval["id"],
            json!({"decision":if allow {"decline"} else {"accept"}}),
        )
        .await?;
        completed(rpc, &mut events, &turn).await?;
        let command = events
            .iter()
            .find(|e| {
                e["method"] == "item/completed" && e["params"]["item"]["type"] == "commandExecution"
            })
            .context("missing native command result")?;
        if allow {
            ensure!(
                command["params"]["item"]["exitCode"] == 0
                    && command["params"]["item"]["aggregatedOutput"]
                        .as_str()
                        .is_some_and(|s| s.contains("SYNTHETIC")),
                "allowed literal command did not execute"
            );
        } else {
            ensure!(
                command["params"]["item"]["status"] == "declined",
                "denied command was not declined"
            );
        }
        let completion = replay_proof(proof, &events)?;
        ensure!(
            completion.withheld.is_none() && completion.reply.as_deref() == Some("SYNTHETIC_REPLY"),
            "approval task lost source proof"
        );
        passed(
            &mut checks,
            if allow {
                "commandApprovalAllow"
            } else {
                "commandApprovalDeny"
            },
            "Two subscribed protocol clients received the same native callback; the chosen decision and native command result agreed.",
        );
        passed(
            &mut checks,
            if allow {
                "resolvedStaleDenyCannotReverseAllow"
            } else {
                "resolvedStaleAllowCannotReverseDeny"
            },
            "The losing client received serverRequest/resolved; its opposite stale response did not reverse the native command result.",
        );
    }
    barrier(rpc, thread).await?;
    control.mode.store(2, Ordering::SeqCst);
    let (turn, proof) = start_case(rpc, thread, Uuid::new_v4(), true).await?;
    let mut events = Vec::new();
    let question = collect_until(rpc, &mut events, |e| {
        e["method"] == "item/tool/requestUserInput" && e["params"]["turnId"] == turn
    })
    .await
    .context("native plan question not observed")?;
    ensure!(
        question["params"]["threadId"] == thread && question["params"]["isBlocking"] == true,
        "question identity or blocking contract mismatch"
    );
    respond(
        rpc,
        &question["id"],
        json!({"answers":{"probe_choice":{"answers":["Yes"]}}}),
    )
    .await?;
    completed(rpc, &mut events, &turn).await?;
    ensure!(
        events
            .iter()
            .any(|e| e["method"] == "serverRequest/resolved"
                && e["params"]["requestId"] == question["id"]),
        "question never resolved"
    );
    let completion = replay_proof(proof, &events)?;
    ensure!(
        completion.withheld.is_none(),
        "question source proof failed"
    );
    passed(
        &mut checks,
        "planQuestionRoundTrip",
        "A native blocking plan-mode question accepted an answer for its exact request and resolved before turn completion.",
    );

    // Own queue remains serialized behind work submitted by the other native client.
    barrier(rpc, thread).await?;
    control.mode.store(3, Ordering::SeqCst);
    let before = control.entered.load(Ordering::SeqCst);
    let (local_turn, _) = start_case(&mut second, thread, Uuid::new_v4(), false).await?;
    wait_model(control, before).await?;
    let own = Uuid::new_v4();
    let proof = OriginProof::new(thread, own).unwrap();
    let queued=rpc.call("thread/queue/add",json!({"threadId":thread,"clientUserMessageId":own.to_string(),"input":[{"type":"text","text":"SYNTHETIC_QUEUED"}]})).await?;
    let queue = rpc
        .call("thread/queue/list", json!({"threadId":thread}))
        .await?;
    ensure!(
        queue["data"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["id"] == queued["queuedSubmission"]["id"])),
        "busy turn did not retain queued input"
    );
    control.mode.store(0, Ordering::SeqCst);
    control.release.notify_one();
    let mut events = Vec::new();
    let mut local_done = false;
    let mut own_turn = None;
    loop {
        let e = rpc.event().await?;
        if e["method"] == "turn/completed" && e["params"]["turn"]["id"] == local_turn {
            local_done = true;
        }
        if e["method"] == "item/started" && e["params"]["item"]["clientId"] == own.to_string() {
            ensure!(
                local_done,
                "queued user started before local turn completed"
            );
            own_turn = e["params"]["turnId"].as_str().map(str::to_owned);
        }
        let done = e["method"] == "turn/completed"
            && own_turn.as_deref() == e["params"]["turn"]["id"].as_str();
        events.push(e);
        ensure!(events.len() < 2048, "queue capture bound");
        if done {
            break;
        }
    }
    let completion = replay_proof(proof, &events)?;
    ensure!(completion.withheld.is_none(), "queued source proof failed");
    passed(
        &mut checks,
        "queueWaitsBehindOtherClientTurn",
        "The own input remained in the native queue while another client's model was held; its user lifecycle started only after that turn completed.",
    );

    for late in [false, true] {
        barrier(rpc, thread).await?;
        control
            .mode
            .store(if late { 4 } else { 3 }, Ordering::SeqCst);
        let before = control.entered.load(Ordering::SeqCst);
        let (turn, proof) = start_case(rpc, thread, Uuid::new_v4(), false).await?;
        wait_model(control, before).await?;
        let mut events = Vec::new();
        if late {
            collect_until(rpc, &mut events, |e| {
                e["method"] == "item/completed"
                    && e["params"]["item"]["type"] == "agentMessage"
                    && e["params"]["turnId"] == turn
            })
            .await?;
            ensure!(
                !events
                    .iter()
                    .any(|e| e["method"] == "turn/completed" && e["params"]["turn"]["id"] == turn),
                "late input was not before completion"
            );
        }
        let steering=second.call("turn/steer",json!({"threadId":thread,"expectedTurnId":turn,"clientUserMessageId":Uuid::new_v4().to_string(),"input":[{"type":"text","text":"SYNTHETIC_OTHER_CLIENT"}]})).await?;
        ensure!(steering["turnId"] == turn, "steer changed turn identity");
        control.mode.store(0, Ordering::SeqCst);
        control.release.notify_one();
        completed(rpc, &mut events, &turn).await?;
        let completion = replay_proof(proof, &events)?;
        ensure!(
            completion.withheld == Some(Withheld::MixedInput) && completion.reply.is_none(),
            "mixed native input escaped origin gate"
        );
        passed(
            &mut checks,
            if late {
                "lateSteerAfterFinalWithheld"
            } else {
                "otherClientSteerWithheld"
            },
            "A distinct native client message entered the same turn; OriginProof withheld the final answer and reported MixedInput.",
        );
    }

    // An intentionally unread submit response is not a license to resend.
    barrier(rpc, thread).await?;
    control.mode.store(0, Ordering::SeqCst);
    let own = Uuid::new_v4();
    let mut proof = OriginProof::new(thread, own).unwrap();
    proof.observe(Evidence::Gap);
    let mut lost = Rpc::connect(socket).await?;
    lost.socket.send(Message::Text(json!({"id":999,"method":"thread/queue/add","params":{"threadId":thread,"clientUserMessageId":own.to_string(),"input":[{"type":"text","text":"SYNTHETIC_LOST_ACK"}]}}).to_string().into())).await?;
    // Do not read the response. Close after a brief transport window, never retry.
    tokio::time::sleep(Duration::from_millis(150)).await;
    drop(lost);
    let mut events = Vec::new();
    let user = collect_until(rpc, &mut events, |e| {
        e["method"] == "item/started" && e["params"]["item"]["clientId"] == own.to_string()
    })
    .await?;
    let turn = user["params"]["turnId"]
        .as_str()
        .context("missing lost-ack turn")?;
    completed(rpc, &mut events, turn).await?;
    let completion = replay_proof(proof, &events)?;
    ensure!(
        completion.reply.is_none() && completion.withheld == Some(Withheld::CaptureGap),
        "capture-gap attempt released content"
    );
    let history = rpc
        .call(
            "thread/read",
            json!({"threadId":thread,"includeTurns":true}),
        )
        .await?;
    let users = history["thread"]["turns"]
        .as_array()
        .context("history missing")?
        .iter()
        .flat_map(|t| t["items"].as_array().into_iter().flatten())
        .filter(|i| i["type"] == "userMessage" && i["clientId"] == own.to_string())
        .count();
    ensure!(users == 1, "unread-ACK submission repeated");
    passed(
        &mut checks,
        "unreadAckNoReplayAndGapWithheld",
        "One request was sent, its acknowledgment was deliberately unread, and native history contained one matching user item; a capture-gap proof withheld output.",
    );

    // Screen bytes verify only the draft UI; all task ownership remains semantic.
    barrier(rpc, thread).await?;
    let handle = native.handle();
    let attachment = handle.attach().await?;
    let grant = handle.claim(attachment.connection_id).await?;
    handle
        .input(
            attachment.connection_id,
            grant.generation,
            1,
            b"\x1b[200~SYNTHETIC_UNSENT_DRAFT\x1b[201~".to_vec(),
        )
        .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let own = Uuid::new_v4();
    let proof = OriginProof::new(thread, own).unwrap();
    rpc.call("thread/queue/add",json!({"threadId":thread,"clientUserMessageId":own.to_string(),"input":[{"type":"text","text":"SYNTHETIC_WITH_DRAFT"}]})).await?;
    let mut events = Vec::new();
    let user = collect_until(rpc, &mut events, |e| {
        e["method"] == "item/started" && e["params"]["item"]["clientId"] == own.to_string()
    })
    .await?;
    completed(rpc, &mut events, user["params"]["turnId"].as_str().unwrap()).await?;
    let completion = replay_proof(proof, &events)?;
    ensure!(completion.withheld.is_none(), "draft queue proof failed");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let snapshot = handle.attach().await?;
    let mut parser = vt100::Parser::new(45, 120, 0);
    parser.process(&snapshot.snapshot.screen);
    parser.process(&snapshot.snapshot.replay);
    ensure!(
        parser
            .screen()
            .contents()
            .contains("SYNTHETIC_UNSENT_DRAFT"),
        "native draft was not preserved"
    );
    handle
        .input(
            attachment.connection_id,
            grant.generation,
            2,
            b"\x15".to_vec(),
        )
        .await?;
    handle
        .release(attachment.connection_id, grant.generation)
        .await?;
    passed(
        &mut checks,
        "semanticQueuePreservesNativeDraft",
        "The native unsent draft remained visible after a semantic queue task completed; no PTY text was injected for that task.",
    );
    Ok(checks)
}

async fn tee_event(
    capture: &mut codex_local_observer::workbench::channel::native::CaptureReceiver,
) -> Result<Value> {
    use codex_local_observer::workbench::channel::native::Direction;
    loop {
        ensure!(
            capture.state().gap_generation == 0 && !capture.state().closed,
            "native tee capture incomplete"
        );
        let frame = tokio::time::timeout(Duration::from_secs(15), capture.recv())
            .await
            .context("TUI subscription event deadline")?
            .context("TUI tee closed")?;
        let current = capture.state();
        ensure!(
            current.gap_generation == 0
                && !current.closed
                && current.connection_epoch == Some(frame.connection_epoch),
            "native tee qualification changed while waiting"
        );
        if frame.direction == Direction::ToTui {
            return serde_json::from_slice(&frame.bytes).context("native tee JSON unsupported");
        }
    }
}
async fn tee_completion(
    rpc: &mut Rpc,
    capture: &mut codex_local_observer::workbench::channel::native::CaptureReceiver,
    mut proof: OriginProof,
    turn: &str,
    thread: &str,
    interaction: usize,
) -> Result<()> {
    let mut saw_approval = false;
    let mut saw_command_result = false;
    let mut saw_question = false;
    let mut question_id = None;
    let mut question_resolved = false;
    let mut semantic = [false; 5];
    loop {
        let event = tee_event(capture).await?;
        if event["method"] == "item/commandExecution/requestApproval"
            && event["params"]["turnId"] == turn
        {
            ensure!(
                interaction == 1 && event["params"]["threadId"] == thread,
                "unexpected native approval"
            );
            saw_approval = true;
            respond(rpc, &event["id"], json!({"decision":"accept"})).await?;
        }
        if event["method"] == "item/completed"
            && event["params"]["turnId"] == turn
            && event["params"]["item"]["type"] == "commandExecution"
        {
            ensure!(
                event["params"]["item"]["status"] == "completed"
                    && event["params"]["item"]["exitCode"] == 0
                    && event["params"]["item"]["aggregatedOutput"]
                        .as_str()
                        .is_some_and(|s| s.contains("SYNTHETIC")),
                "unsubscribed control approval did not execute literal command"
            );
            saw_command_result = true;
        }
        if event["method"] == "item/tool/requestUserInput" && event["params"]["turnId"] == turn {
            ensure!(
                interaction == 2
                    && event["params"]["threadId"] == thread
                    && event["params"]["isBlocking"] == true,
                "unexpected native question"
            );
            saw_question = true;
            question_id = Some(event["id"].clone());
            respond(
                rpc,
                &event["id"],
                json!({"answers":{"probe_choice":{"answers":["Yes"]}}}),
            )
            .await?;
        }
        if event["method"] == "serverRequest/resolved"
            && question_id.as_ref() == Some(&event["params"]["requestId"])
        {
            question_resolved = true;
        }
        if let Some(evidence) = native_evidence(&event)? {
            match evidence {
                Evidence::TurnStarted { turn: id, .. } if id == turn => semantic[0] = true,
                Evidence::UserStarted { turn: id, .. } if id == turn => semantic[1] = true,
                Evidence::UserCompleted { turn: id, .. } if id == turn => semantic[2] = true,
                Evidence::FinalAnswer { turn: id, .. } if id == turn => semantic[3] = true,
                Evidence::TurnCompleted { turn: id, .. } if id == turn => semantic[4] = true,
                _ => {}
            }
            if let Some(completion) = proof.observe(evidence) {
                ensure!(
                    semantic.iter().all(|seen| *seen),
                    "TUI subscription omitted required native lifecycle"
                );
                ensure!(
                    completion.status == NativeStatus::Completed
                        && completion.withheld.is_none()
                        && completion.reply.as_deref() == Some("SYNTHETIC_REPLY"),
                    "TUI tee origin gate withheld completion"
                );
                ensure!(
                    interaction != 1 || (saw_approval && saw_command_result),
                    "approval execution evidence missing on TUI tee"
                );
                ensure!(
                    interaction != 2 || (saw_question && question_resolved),
                    "question completion evidence missing on TUI tee"
                );
                return Ok(());
            }
        }
    }
}
async fn read_turn_completed(rpc: &mut Rpc, thread: &str, turn: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let history = rpc
                .call(
                    "thread/read",
                    json!({"threadId":thread,"includeTurns":true}),
                )
                .await?;
            if history["thread"]["turns"].as_array().is_some_and(|turns| {
                turns
                    .iter()
                    .any(|t| t["id"] == turn && t["status"] == "completed")
            }) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("read-only native turn completion deadline")?
}
async fn run_attachment_boundaries(
    rpc: &mut Rpc,
    mut capture: codex_local_observer::workbench::channel::native::CaptureReceiver,
    native: &TerminalHost,
    thread: &str,
    control: &Arc<BoundaryModel>,
) -> Result<Vec<Value>> {
    let mut checks = Vec::new();
    // Consume the already completed local turn on the TUI connection before a new
    // submission attempt. The control client has never called resume or queue.
    loop {
        let e = tee_event(&mut capture).await?;
        if e["method"] == "turn/completed" && e["params"]["threadId"] == thread {
            break;
        }
    }
    for interaction in [0, 1, 2] {
        control.mode.store(interaction, Ordering::SeqCst);
        let (turn, proof) = start_case(rpc, thread, Uuid::new_v4(), interaction == 2).await?;
        tee_completion(rpc, &mut capture, proof, &turn, thread, interaction).await?;
        passed(
            &mut checks,
            match interaction {
                1 => "approvalViaTuiSubscriptionWithoutControlResume",
                2 => "questionViaTuiSubscriptionWithoutControlResume",
                _ => "originViaTuiSubscriptionWithoutControlResume",
            },
            "The existing TUI subscription supplied full native lifecycle evidence; an independent turn/start control client performed no resume or queue mutation.",
        );
    }
    // Deliberately remove the capture consumer while a real model stream is held.
    // Forwarding must finish; loss invalidates this exact proof, never resumes it.
    control.mode.store(3, Ordering::SeqCst);
    let before = control.entered.load(Ordering::SeqCst);
    let client = Uuid::new_v4();
    let (turn, mut proof) = start_case(rpc, thread, client, false).await?;
    wait_model(control, before).await?;
    loop {
        let e = tee_event(&mut capture).await?;
        let own =
            e["method"] == "item/started" && e["params"]["item"]["clientId"] == client.to_string();
        if let Some(evidence) = native_evidence(&e)? {
            ensure!(
                proof.observe(evidence).is_none(),
                "held model completed early"
            );
        }
        if own {
            break;
        }
    }
    let mut state = capture.subscribe_state();
    drop(capture);
    control.mode.store(0, Ordering::SeqCst);
    control.release.notify_one();
    tokio::time::timeout(Duration::from_secs(15), async {
        while state.state().gap_generation == 0 {
            state.changed().await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    ensure!(
        proof.observe(Evidence::Gap).is_none() && proof.invalidated() == Some(Withheld::CaptureGap),
        "loss did not invalidate proof"
    );
    read_turn_completed(rpc, thread, &turn).await?;
    // No later history summary is fed to the proof to restore qualification.
    passed(
        &mut checks,
        "captureConsumerLossDoesNotBlockTuiAndInvalidatesProof",
        "Dropping observation caused a real out-of-band capture gap; the original task still completed in native history while its proof stayed invalidated.",
    );

    native.handle().stop().await?;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let loaded = rpc.call("thread/loaded/list", json!({})).await?;
            if loaded["data"]
                .as_array()
                .is_some_and(|ids| !ids.iter().any(|id| id == thread))
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("target unload deadline")??;
    let client = Uuid::new_v4();
    let rejected=rpc.call_envelope("turn/start",json!({"threadId":thread,"clientUserMessageId":client.to_string(),"input":[{"type":"text","text":"SYNTHETIC_MUST_NOT_QUEUE"}]})).await?;
    ensure!(
        rejected["error"]["code"] == -32600
            && rejected["error"]["message"]
                .as_str()
                .is_some_and(|s| s.contains("thread not found")),
        "unloaded target did not reject turn/start as missing"
    );
    let queue = rpc
        .call("thread/queue/list", json!({"threadId":thread}))
        .await?;
    ensure!(
        queue["data"].as_array().is_some_and(Vec::is_empty),
        "rejected turn created future queued work"
    );
    let loaded = rpc.call("thread/loaded/list", json!({})).await?;
    ensure!(
        loaded["data"]
            .as_array()
            .is_some_and(|ids| !ids.iter().any(|id| id == thread)),
        "rejected turn cold-loaded target"
    );
    passed(
        &mut checks,
        "unloadedTurnStartRejectsWithoutColdLoadOrQueue",
        "After the TUI disconnected and the target unloaded, turn/start returned thread-not-found; loaded state stayed absent and native queue stayed empty.",
    );
    Ok(checks)
}
