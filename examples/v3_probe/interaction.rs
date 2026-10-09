//! P2 mechanism probe only. No Telegram, resume, queue, real model, or user files.
use super::*;
use std::os::unix::{
    fs::{FileTypeExt, MetadataExt},
    process::CommandExt,
};

// The wrapper owns a fresh process group. The socket's kernel peer must belong
// to that group before it receives a shutdown signal. Never unlink the official
// shared socket parent, enumerate/kill unrelated Codex processes, or use pkill.
struct PeerOwner {
    child: std::process::Child,
    peer: Option<PeerIdentity>,
    reaped: bool,
    socket: Option<OwnedSocket>,
}
struct OwnedSocket {
    path: PathBuf,
    uid: u32,
    device: u64,
    inode: u64,
}
impl OwnedSocket {
    fn capture(alias: &Path) -> Result<Self> {
        let path = alias.canonicalize()?;
        let metadata = std::fs::symlink_metadata(&path)?;
        let advertised = std::fs::metadata(alias)?;
        ensure!(
            metadata.file_type().is_socket()
                && metadata.uid() == unsafe { libc::geteuid() }
                && advertised.dev() == metadata.dev()
                && advertised.ino() == metadata.ino(),
            "socket identity mismatch"
        );
        Ok(Self {
            path,
            uid: metadata.uid(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    // Only called after the verified peer is gone. Never remove its parent or lock.
    fn clean(&self) -> bool {
        match std::fs::symlink_metadata(&self.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Ok(metadata)
                if metadata.file_type().is_socket()
                    && metadata.uid() == self.uid
                    && metadata.dev() == self.device
                    && metadata.ino() == self.inode =>
            {
                std::fs::remove_file(&self.path).is_ok()
            }
            _ => false,
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct PeerIdentity {
    pid: i32,
    uid: u32,
    #[cfg(target_os = "macos")]
    started: (u64, u64),
}
#[cfg(target_os = "macos")]
fn process_info(pid: i32) -> Option<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (read == size).then_some(info)
}
impl PeerOwner {
    fn verify(&mut self, rpc: &Rpc) -> Result<()> {
        let credentials = rpc.socket.get_ref().peer_cred()?;
        let peer = credentials.pid().context("peer pid unavailable")?;
        ensure!(
            credentials.uid() == unsafe { libc::geteuid() },
            "peer uid mismatch"
        );
        ensure!(
            unsafe { libc::getpgid(peer) } == self.child.id() as i32,
            "peer outside owned group"
        );
        #[cfg(target_os = "macos")]
        let started = {
            let info = process_info(peer).context("peer identity unavailable")?;
            ensure!(
                info.pbi_uid == credentials.uid() && info.pbi_pgid == self.child.id(),
                "peer process identity mismatch"
            );
            (info.pbi_start_tvsec, info.pbi_start_tvusec)
        };
        let identity = PeerIdentity {
            pid: peer,
            uid: credentials.uid(),
            #[cfg(target_os = "macos")]
            started,
        };
        ensure!(
            self.peer.is_none_or(|prior| prior == identity),
            "native peer identity changed"
        );
        self.peer = Some(identity);
        Ok(())
    }
    async fn stop(&mut self, rpc: &mut Rpc, thread: Option<&str>) -> Result<()> {
        self.verify(rpc)?;
        let peer = self.peer.context("missing owned peer")?.pid;
        // Follow the native shutdown lifecycle, then help interrupt any request
        // still pending after a failed assertion. This does not claim tool cleanup.
        ensure!(
            unsafe { libc::kill(peer, libc::SIGTERM) } == 0,
            "peer shutdown signal failed"
        );
        if let Some(thread) = thread {
            let _ = tokio::time::timeout(Duration::from_secs(3), async {
                let history = rpc
                    .call(
                        "thread/read",
                        json!({"threadId":thread,"includeTurns":true}),
                    )
                    .await?;
                for turn in history["thread"]["turns"].as_array().into_iter().flatten() {
                    if turn["status"] == "inProgress" {
                        rpc.call(
                            "turn/interrupt",
                            json!({"threadId":thread,"turnId":turn["id"]}),
                        )
                        .await?;
                    }
                }
                rpc.call(
                    "thread/backgroundTerminals/clean",
                    json!({"threadId":thread}),
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
            .await;
        }
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                // WNOWAIT keeps the group leader unreaped until the peer is
                // known gone. Failure therefore leaves Drop a stable owned group.
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                ensure!(
                    unsafe {
                        libc::waitid(
                            libc::P_PID,
                            self.child.id() as libc::id_t,
                            &mut info,
                            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                        )
                    } == 0,
                    "owned wrapper wait observation failed"
                );
                if unsafe { info.si_pid() } == self.child.id() as i32 {
                    ensure!(
                        info.si_code == libc::CLD_EXITED && unsafe { info.si_status() } == 0,
                        "owned wrapper exited unsuccessfully"
                    );
                    let peer_gone = peer == self.child.id() as i32
                        || (unsafe { libc::kill(peer, 0) } != 0
                            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH));
                    if peer_gone {
                        let status = self.child.wait()?;
                        self.reaped = true;
                        ensure!(status.success(), "owned wrapper reap failed");
                        ensure!(
                            self.socket.as_ref().is_none_or(OwnedSocket::clean),
                            "owned socket leaf cleanup unknown"
                        );
                        return Ok::<_, anyhow::Error>(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .context("owned peer shutdown deadline")?
    }
}
impl Drop for PeerOwner {
    fn drop(&mut self) {
        if !self.reaped {
            // The unreaped group leader prevents its pid from being reused.
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            if self.child.wait().is_ok() {
                self.reaped = true;
            }
        }
        if let Some(socket) = &self.socket {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            let peer_gone = loop {
                let gone = self.reaped
                    && self.peer.is_some_and(|peer| {
                        peer.pid == self.child.id() as i32
                            || (unsafe { libc::kill(peer.pid, 0) } != 0
                                && std::io::Error::last_os_error().raw_os_error()
                                    == Some(libc::ESRCH))
                    });
                if gone || std::time::Instant::now() >= deadline {
                    break gone;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            if !peer_gone || !socket.clean() {
                eprintln!(
                    "{}",
                    json!({"check":"ownedSocketLeafCleanup","known":false})
                );
            }
        }
    }
}

#[derive(Default)]
struct Model {
    mode: AtomicUsize, // 0 plain; 1 command; 2 file; 3 question
    // All retained values are from this literal synthetic question only.
    answers: Mutex<Vec<Value>>,
    calls: Mutex<HashMap<String, String>>,
}
fn title_response() -> Response {
    let item = json!({"type":"message","role":"assistant","id":"synthetic_title_message","content":[{"type":"output_text","text":"{\"title\":\"Synthetic interaction probe\"}"}]});
    let bytes = [
        event(json!({"type":"response.created","response":{"id":"synthetic_title_response"}})),
        event(json!({"type":"response.output_item.done","output_index":0,"item":item})),
        event(json!({"type":"response.completed","response":{"id":"synthetic_title_response"}})),
    ]
    .concat();
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(bytes))
        .unwrap()
}
fn model_response(index: usize, first: bool, mode: usize) -> Response {
    let stream = async_stream::stream! {
        let id = format!("race_response_{index}");
        let call_id = format!("race_call_{index}");
        yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":id,"model":"gpt-6-astra"}})));
        let tool = if first { match mode {
            1 => Some(("exec_command", json!({"cmd":"printf SYNTHETIC_RACE_COMMAND","login":false,"sandbox_permissions":"require_escalated","justification":"Synthetic literal-output race probe","yield_time_ms":1000}))),
            // Core intercepts this literal apply_patch heredoc; the only target is
            // an absent file in the fresh temporary workspace under readOnly.
            2 => Some(("exec_command", json!({"cmd":"apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: synthetic-race.txt\n+SYNTHETIC_PATCH\n*** End Patch\nPATCH","login":false,"yield_time_ms":1000}))),
            3 => Some(("request_user_input", json!({"questions":[{"id":"probe_choice","header":"Probe","question":"Choose the synthetic option","options":[{"label":"Yes","description":"Synthetic yes"},{"label":"No","description":"Synthetic no"}]}]}))),
            _ => None,
        }} else { None };
        let item = if let Some((name, arguments)) = tool {
            json!({"type":"function_call","id":call_id,"call_id":call_id,"name":name,"arguments":arguments.to_string()})
        } else {
            json!({"type":"message","id":format!("race_message_{index}"),"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"SYNTHETIC_REPLY"}]})
        };
        yield Ok(event(json!({"type":"response.output_item.added","output_index":0,"item":item})));
        yield Ok(event(json!({"type":"response.output_item.done","output_index":0,"item":item})));
        yield Ok(event(json!({"type":"response.completed","response":{"id":id}})));
    };
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

pub(super) async fn run(report_path: Option<&Path>) -> Result<()> {
    let mut checks = Vec::new();
    let mut stage = "startup";
    let mut step = "startup";
    let result = suite(&mut checks, &mut stage, &mut step).await;
    let report = json!({"schemaVersion":1,"kind":"native-interaction-races","candidateOnly":true,"passed":result.is_ok(),"failedStage":if result.is_ok(){None}else{Some(stage)},"failedStep":if result.is_ok(){None}else{Some(step)},"sourceCommit":"2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4","checks":checks,"notTested":["Production Telegram authorization, callback delivery, or task attribution","Actual human TUI keystrokes racing a remote callback","Exact arrival order or client winner acknowledgment","Installed binary correspondence to research source commit","Detached arbitrary tool cleanup"]});
    if let Some(path) = report_path {
        write_report(path, &report)?;
    }
    println!("{report}");
    // Suppress underlying payload, path, and native identifiers on all failures.
    ensure!(
        result.is_ok(),
        "native interaction candidate failed at {stage}; see aggregate checks"
    );
    Ok(())
}

async fn suite(
    checks: &mut Vec<Value>,
    stage: &mut &'static str,
    step: &mut &'static str,
) -> Result<()> {
    let directory = tempfile::Builder::new()
        .prefix("v3race-")
        .tempdir_in("/tmp")?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    let home = directory.path().join("home");
    let temporary = home.join("tmp");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir_all(&temporary)?;
    std::fs::create_dir(&workspace)?;
    let workspace = workspace.canonicalize()?;
    let model = Arc::new(Model::default());
    let control = model.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/v1", listener.local_addr()?);
    let counts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/responses",
                post(move |request: axum::extract::Request| {
                    let control = control.clone();
                    let counts = counts.clone();
                    async move {
                        let header = request
                            .headers()
                            .get("x-codex-turn-metadata")
                            .and_then(|h| h.to_str().ok())
                            .unwrap_or("{}")
                            .to_owned();
                        let bytes = axum::body::to_bytes(request.into_body(), 16 * 1024 * 1024)
                            .await
                            .unwrap();
                        let body: Value = serde_json::from_slice(&bytes).unwrap();
                        if body
                            .pointer("/text/format/schema/properties/title")
                            .is_some()
                        {
                            return title_response();
                        }
                        let metadata: Value = serde_json::from_str(
                            body["client_metadata"]["x-codex-turn-metadata"]
                                .as_str()
                                .unwrap_or(&header),
                        )
                        .unwrap();
                        let turn = metadata["turn_id"].as_str().unwrap_or("missing");
                        let (n, index) = {
                            let mut counts = counts.lock().unwrap();
                            let index = counts.values().sum::<usize>();
                            let count = counts.entry(turn.to_owned()).or_default();
                            *count += 1;
                            (*count, index)
                        };
                        if n == 1 {
                            control
                                .calls
                                .lock()
                                .unwrap()
                                .insert(turn.to_owned(), format!("race_call_{index}"));
                        }
                        if control.mode.load(Ordering::SeqCst) == 3 && n == 2 {
                            let call = control.calls.lock().unwrap().get(turn).cloned();
                            // Select only this turn's literal tool call output, not old
                            // answers accumulated in the native thread's model context.
                            for item in body["input"].as_array().into_iter().flatten() {
                                if item["type"] == "function_call_output"
                                    && item["call_id"].as_str() == call.as_deref()
                                    && let Some(text) = item["output"].as_str()
                                    && let Ok(value) = serde_json::from_str::<Value>(text)
                                    && value.get("answers").is_some()
                                {
                                    control.answers.lock().unwrap().push(value);
                                }
                            }
                        }
                        model_response(index, n == 1, control.mode.load(Ordering::SeqCst))
                    }
                }),
            ),
        )
        .await
    });
    // Abort the fixture server even when startup fails.
    struct Server(tokio::task::JoinHandle<std::io::Result<()>>);
    impl Drop for Server {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Server(server);
    config(&home, &workspace, &url)?;
    let config_path = home.join("config.toml");
    let prior = std::fs::read_to_string(&config_path)?;
    std::fs::write(
        config_path,
        format!(
            "thread_unload_delay_secs=0\napproval_policy=\"on-request\"\napprovals_reviewer=\"user\"\nsandbox_mode=\"read-only\"\n{prior}"
        ),
    )?;
    let binary = std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into());
    let version = tokio::process::Command::new(&binary)
        .arg("--version")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("CODEX_HOME", &home)
        .env("TMPDIR", &temporary)
        .output()
        .await?;
    ensure!(version.status.success(), "version probe failed");
    // The expected installation is an explicit qualification, never upgraded.
    ensure!(
        String::from_utf8_lossy(&version.stdout).trim() == "codex-cli 0.156.1",
        "unqualified CLI version"
    );
    passed(
        checks,
        "installedCli01561",
        "The isolated CLI reported codex-cli 0.156.1; source correspondence is unproven.",
    );
    let socket = directory.path().join("rpc.sock");
    let child = std::process::Command::new(&binary)
        .args(["app-server", "--listen"])
        .arg(format!("unix://{}", socket.display()))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("CODEX_HOME", &home)
        .env("TMPDIR", &temporary)
        .current_dir(&workspace)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut owner = PeerOwner {
        child,
        peer: None,
        reaped: false,
        socket: None,
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        while !socket.exists() {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .context("socket startup deadline")?;
    let mut rpc = Rpc::connect(&socket).await?;
    owner.verify(&rpc)?;
    owner.socket = Some(OwnedSocket::capture(&socket)?);
    let (relay, mut capture) = codex_local_observer::workbench::channel::native::Relay::bind(
        directory.path().canonicalize()?.join("tui.sock"),
        socket.canonicalize()?,
        Default::default(),
    )
    .await?;
    let native = tui(
        &binary,
        &home,
        &workspace,
        &[
            "--remote".into(),
            format!("unix://{}", relay.path().display()),
        ],
    )?;
    let mut thread = None;
    let result = tokio::time::timeout(Duration::from_secs(180), async {
        initialize_tui(&native, "SYNTHETIC_LOCAL").await?;
        loop {
            let e = tee_event(&mut capture).await?;
            if e["method"] == "turn/completed" {
                thread = Some(
                    e["params"]["threadId"]
                        .as_str()
                        .context("local thread missing")?
                        .to_owned(),
                );
                break;
            }
        }
        let thread = thread.as_deref().unwrap();
        let mut local = Rpc::connect(&socket).await?;
        owner.verify(&local)?;
        run_cases(
            &mut rpc,
            &mut local,
            &mut capture,
            CaseContext {
                thread,
                workspace: &workspace,
                model: &model,
                checks,
                stage,
                step,
            },
        )
        .await
    })
    .await;
    let _ = native.handle().stop().await;
    drop(native);
    drop(relay);
    let stopped = owner.stop(&mut rpc, thread.as_deref()).await;
    if stopped.is_ok() {
        passed(
            checks,
            "ownedNativePeerExited",
            "Verified native peer exited and its owned wrapper was reaped; shared parent directories were preserved.",
        );
    }
    result.context("suite deadline")??;
    *stage = "cleanup";
    *step = "peer-and-socket-cleanup";
    stopped?;
    Ok(())
}

async fn observe_until(
    capture: &mut codex_local_observer::workbench::channel::native::CaptureReceiver,
    events: &mut Vec<Value>,
    predicate: impl Fn(&Value) -> bool,
) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            ensure!(events.len() < 2048, "event bound exceeded");
            let e = tee_event(capture).await?;
            let matched = predicate(&e);
            events.push(e.clone());
            if matched {
                return Ok(e);
            }
        }
    })
    .await
    .context("interaction event deadline")?
}

// Two independent native transports are released by the same rendezvous. Neither
// waits for resolution or the other's write. A flush success is NOT a native ACK.
async fn race(left: &mut Rpc, right: &mut Rpc, id: &Value, a: Value, b: Value) -> Result<()> {
    let gate = tokio::sync::Barrier::new(2);
    let (a, b) = tokio::join!(
        async {
            gate.wait().await;
            respond(left, id, a).await
        },
        async {
            gate.wait().await;
            respond(right, id, b).await
        }
    );
    a?;
    b?;
    Ok(())
}
fn answer(text: &str) -> Value {
    json!({"answers":{"probe_choice":{"answers":[text]}}})
}

struct CaseContext<'a> {
    thread: &'a str,
    workspace: &'a Path,
    model: &'a Arc<Model>,
    checks: &'a mut Vec<Value>,
    stage: &'a mut &'static str,
    step: &'a mut &'static str,
}
async fn run_cases(
    control: &mut Rpc,
    local: &mut Rpc,
    capture: &mut codex_local_observer::workbench::channel::native::CaptureReceiver,
    context: CaseContext<'_>,
) -> Result<()> {
    let CaseContext {
        thread,
        workspace,
        model,
        checks,
        stage,
        step,
    } = context;
    // Control/local are separate protocol clients; neither changes subscriptions.
    // The unchanged TUI connection supplies all observation and source evidence.
    for (case, mode, response_kind) in [
        ("commandConcurrentAllowDeny", 1, "race"),
        ("commandConcurrentDenyAllow", 1, "reverse-race"),
        ("fileApprovalAllow", 2, "allow"),
        ("fileApprovalDeny", 2, "deny"),
        ("fileConcurrentAllowDeny", 2, "race"),
        ("questionConcurrentDifferentAnswers", 3, "race"),
        ("questionStaleIdDoesNotConsumeCurrent", 3, "stale"),
        ("questionMalformedFirstConsumesRequest", 3, "malformed"),
        ("questionUnknownIdPassesThrough", 3, "unknown"),
    ] {
        *stage = case;
        *step = "case-start";
        let file = workspace.join("synthetic-race.txt");
        if file.exists() {
            ensure!(
                std::fs::read(&file)? == b"SYNTHETIC_PATCH\n",
                "unexpected synthetic file content"
            );
            std::fs::remove_file(&file)?;
        }
        model.mode.store(mode, Ordering::SeqCst);
        model.answers.lock().unwrap().clear();
        let client = Uuid::new_v4();
        *step = "turn-start";
        let (turn, mut proof) = start_case(control, thread, client, mode == 3).await?;
        proof.observe(Evidence::SubmissionTurn(&turn));
        let method = match mode {
            1 => "item/commandExecution/requestApproval",
            2 => "item/fileChange/requestApproval",
            _ => "item/tool/requestUserInput",
        };
        let mut events = Vec::new();
        *step = "request-seen";
        let request_result = observe_until(capture, &mut events, |e| {
            e["method"] == method && e["params"]["turnId"] == turn
        })
        .await;
        let diagnostic = json!({"name":"requestObservation","case":case,
            "matchedRequest":request_result.is_ok(),
            "commandApprovalSeen":events.iter().any(|e|e["method"]=="item/commandExecution/requestApproval"),
            "fileApprovalSeen":events.iter().any(|e|e["method"]=="item/fileChange/requestApproval"),
            "questionSeen":events.iter().any(|e|e["method"]=="item/tool/requestUserInput"),
            "turnCompletedBeforeRequest":events.iter().any(|e|e["method"]=="turn/completed" && e["params"]["turn"]["id"]==turn)});
        println!("{diagnostic}");
        checks.push(diagnostic);
        let request = request_result?;
        *step = "identity";
        let fixture_call = model.calls.lock().unwrap().get(&turn).cloned();
        let diagnostic = json!({"name":"interactionIdentity","case":case,
            "threadMatches":request["params"]["threadId"]==thread,
            "requestIdPresent":request.get("id").is_some(),
            "itemIdPresent":request["params"]["itemId"].is_string(),
            "fixtureCallKnown":fixture_call.is_some(),
            "itemMatchesFixtureCall":request["params"]["itemId"].as_str()==fixture_call.as_deref()});
        println!("{diagnostic}");
        checks.push(diagnostic);
        ensure!(
            request["params"]["threadId"] == thread && request.get("id").is_some(),
            "interaction identity mismatch"
        );
        ensure!(
            request["params"]["itemId"].as_str() == fixture_call.as_deref(),
            "interaction item does not match synthetic tool call"
        );
        *step = "live-origin";
        let client_text = client.to_string();
        let target_event = |event: &&Value| event["params"]["threadId"] == thread;
        let target_turn = |event: &&Value| {
            event["params"]["turnId"] == turn || event["params"]["turn"]["id"] == turn
        };
        let agent_completion = |event: &&Value| {
            event["method"] == "item/completed" && event["params"]["item"]["type"] == "agentMessage"
        };
        let unsupported_phase = |event: &&Value| {
            !matches!(
                event["params"]["item"]["phase"].as_str(),
                Some("commentary" | "final_answer")
            )
        };
        let diagnostic = json!({"name":"originCaptureBeforeParse","case":case,
            "capturedEvents":events.len(),
            "otherThreadEvents":events.iter().filter(|e|e["params"]["threadId"].is_string() && e["params"]["threadId"]!=thread).count(),
            "targetTurnStarted":events.iter().filter(target_event).filter(target_turn).filter(|e|e["method"]=="turn/started").count(),
            "targetOwnUserStarted":events.iter().filter(target_event).filter(target_turn).filter(|e|e["method"]=="item/started" && e["params"]["item"]["type"]=="userMessage" && e["params"]["item"]["clientId"]==client_text).count(),
            "targetOwnUserCompleted":events.iter().filter(target_event).filter(target_turn).filter(|e|e["method"]=="item/completed" && e["params"]["item"]["type"]=="userMessage" && e["params"]["item"]["clientId"]==client_text).count(),
            "targetAgentPhaseMissing":events.iter().filter(target_event).filter(target_turn).filter(agent_completion).filter(|e|e["params"]["item"]["phase"].is_null()).count(),
            "targetAgentPhaseUnsupported":events.iter().filter(target_event).filter(target_turn).filter(agent_completion).filter(unsupported_phase).count(),
            "targetTurnCompleted":events.iter().filter(target_event).filter(target_turn).filter(|e|e["method"]=="turn/completed").count(),
            "sameThreadOtherTurnAgentPhaseUnsupported":events.iter().filter(target_event).filter(|e|!target_turn(e)).filter(agent_completion).filter(unsupported_phase).count(),
            "otherThreadAgentPhaseUnsupported":events.iter().filter(|e|!target_event(e)).filter(agent_completion).filter(unsupported_phase).count()});
        println!("{diagnostic}");
        checks.push(diagnostic);
        let mut live = OriginProof::new(thread, client).context("invalid candidate thread")?;
        live.observe(Evidence::SubmissionTurn(&turn));
        for event in events.iter().filter(target_event) {
            // Production decoding qualifies the target thread before parsing its
            // semantics. Unknown phases on this thread remain hard failures.
            let parsed = native_evidence(event);
            if parsed.is_err() {
                let diagnostic = json!({"name":"originParseBoundary","case":case,
                    "parseRejected":true,"targetTurn":target_turn(&event),
                    "agentCompletion":agent_completion(&event),
                    "agentPhaseMissing":agent_completion(&event) && event["params"]["item"]["phase"].is_null(),
                    "agentPhaseUnsupported":agent_completion(&event) && unsupported_phase(&event)});
                println!("{diagnostic}");
                checks.push(diagnostic);
            }
            if let Some(evidence) = parsed? {
                let released = live.observe(evidence).is_some();
                if released {
                    let diagnostic = json!({"name":"originParseBoundary","case":case,
                        "parseRejected":false,"prematureCompletion":true,
                        "targetTurn":target_turn(&event),"terminalEvent":event["method"]=="turn/completed"});
                    println!("{diagnostic}");
                    checks.push(diagnostic);
                }
                ensure!(!released, "turn completed before interaction");
            }
        }
        let diagnostic = json!({"name":"liveOriginPredicates","case":case,
            "liveOwnedTurn":live.live_owned_turn()==Some(turn.as_str()),
            "invalidated":live.invalidated().is_some(),
            "turnStarted":events.iter().any(|e|e["method"]=="turn/started" && e["params"]["turn"]["id"]==turn),
            "ownUserStarted":events.iter().any(|e|e["method"]=="item/started" && e["params"]["turnId"]==turn && e["params"]["item"]["type"]=="userMessage" && e["params"]["item"]["clientId"]==client.to_string()),
            "ownUserCompleted":events.iter().any(|e|e["method"]=="item/completed" && e["params"]["turnId"]==turn && e["params"]["item"]["type"]=="userMessage" && e["params"]["item"]["clientId"]==client.to_string())});
        println!("{diagnostic}");
        checks.push(diagnostic);
        ensure!(
            live.live_owned_turn() == Some(turn.as_str()),
            "interaction lacks live acknowledged origin"
        );
        *step = "interaction-scope";
        let id = &request["id"];
        if mode == 3 {
            ensure!(
                request["params"]["isBlocking"] == true,
                "question is not blocking"
            );
        }
        if mode == 2 {
            ensure!(!file.exists(), "patch executed before approval");
            let scope_known = events.iter().any(|event| {
                let params = &event["params"];
                if params["threadId"] != thread || params["turnId"] != turn {
                    return false;
                }
                let changes = if event["method"] == "item/started"
                    && params["item"]["type"] == "fileChange"
                    && params["item"]["id"] == request["params"]["itemId"]
                {
                    params["item"]["changes"].as_array()
                } else {
                    None
                };
                changes.is_some_and(|changes| {
                    changes.len() == 1
                        && changes[0]["path"].as_str() == file.to_str()
                        && changes[0]["kind"]["type"] == "add"
                })
            });
            let grant_root_absent = request["params"].get("grantRoot") == Some(&Value::Null);
            let scope = json!({"name":"preApprovalFileScope", "case":case,
                "preApprovalFileScopeKnown":scope_known,"grantRootNull":grant_root_absent,
                "status":if scope_known && grant_root_absent {"passed"} else {"incomplete"}});
            println!("{scope}");
            checks.push(scope);
            ensure!(
                scope_known && grant_root_absent,
                "exact single-file approval scope unavailable"
            );
        }
        *step = "responses-written";
        match response_kind {
            "race" | "reverse-race" => {
                let (a, b) = if mode == 3 {
                    (answer("Yes"), answer("No"))
                } else {
                    (json!({"decision":"accept"}), json!({"decision":"decline"}))
                };
                if response_kind == "reverse-race" {
                    race(control, local, id, b, a).await?;
                } else {
                    race(control, local, id, a, b).await?;
                }
            }
            "allow" => respond(control, id, json!({"decision":"accept"})).await?,
            "deny" => respond(local, id, json!({"decision":"decline"})).await?,
            "stale" => {
                // A different unguessable identifier cannot consume this request.
                respond(local, &json!(Uuid::new_v4().to_string()), answer("No")).await?;
                local
                    .call(
                        "thread/read",
                        json!({"threadId":thread,"includeTurns":false}),
                    )
                    .await?;
                ensure!(
                    model.answers.lock().unwrap().is_empty(),
                    "stale identifier consumed current question"
                );
                respond(control, id, answer("Yes")).await?;
            }
            "malformed" => respond(local, id, json!({"answers":[]})).await?,
            "unknown" => respond(
                local,
                id,
                json!({"answers":{"unknown_synthetic_question":{"answers":["SYNTHETIC_UNKNOWN"]}}}),
            )
            .await?,
            _ => unreachable!(),
        }
        *step = "resolved";
        // Wait for the raw callback consumption signal before intentionally late
        // responses. It says neither which client won nor whether its value parsed.
        observe_until(capture, &mut events, |e| {
            e["method"] == "serverRequest/resolved" && e["params"]["requestId"] == *id
        })
        .await?;
        *step = "stale-response-written";
        respond(
            control,
            id,
            if mode == 3 {
                answer("STALE_MUST_NOT_APPEAR")
            } else {
                json!({"decision":"decline"})
            },
        )
        .await?;
        *step = "terminal";
        let done = observe_until(capture, &mut events, |e| {
            e["method"] == "turn/completed" && e["params"]["turn"]["id"] == turn
        })
        .await?;
        ensure!(
            done["params"]["turn"]["status"] == "completed",
            "interaction turn failed"
        );
        ensure!(
            events
                .iter()
                .filter(|e| e["method"] == method && e["params"]["turnId"] == turn)
                .count()
                == 1,
            "interaction was repeated"
        );
        ensure!(
            events
                .iter()
                .filter(
                    |e| e["method"] == "serverRequest/resolved" && e["params"]["requestId"] == *id
                )
                .count()
                == 1,
            "callback resolved more than once"
        );
        *step = "source-proof";
        let diagnostic = json!({"name":"sourceProofPredicates","case":case,
            "explicitFinalItemSeen":events.iter().any(|e|e["method"]=="item/completed" && e["params"]["turnId"]==turn && e["params"]["item"]["type"]=="agentMessage" && e["params"]["item"]["phase"]=="final_answer"),
            "exactlyOneTurnCompleted":events.iter().filter(|e|e["method"]=="turn/completed" && e["params"]["turn"]["id"]==turn).count()==1});
        println!("{diagnostic}");
        checks.push(diagnostic);
        let target_events = events
            .iter()
            .filter(target_event)
            .cloned()
            .collect::<Vec<_>>();
        let completion = replay_proof(proof, &target_events)?;
        ensure!(
            completion.withheld.is_none() && completion.reply.as_deref() == Some("SYNTHETIC_REPLY"),
            "interaction lost original turn source proof"
        );
        *step = "tool-outcome";
        if mode == 1 || mode == 2 {
            let item_type = if mode == 1 {
                "commandExecution"
            } else {
                "fileChange"
            };
            let items = events
                .iter()
                .filter(|e| {
                    e["method"] == "item/completed"
                        && e["params"]["turnId"] == turn
                        && e["params"]["item"]["type"] == item_type
                })
                .collect::<Vec<_>>();
            let diagnostic = json!({"name":"toolOutcomePredicates","case":case,
                "exactlyOneTerminalItem":items.len()==1,
                "completed":items.first().is_some_and(|e|e["params"]["item"]["status"]=="completed"),
                "declined":items.first().is_some_and(|e|e["params"]["item"]["status"]=="declined"),
                "failed":items.first().is_some_and(|e|e["params"]["item"]["status"]=="failed"),
                "successfulExit":items.first().is_some_and(|e|e["params"]["item"]["exitCode"]==0),
                "literalOutputPresent":items.first().is_some_and(|e|e["params"]["item"]["aggregatedOutput"].as_str().is_some_and(|s|s.contains("SYNTHETIC_RACE_COMMAND")))});
            println!("{diagnostic}");
            checks.push(diagnostic);
            ensure!(items.len() == 1, "tool terminal result missing or repeated");
            let item = &items[0]["params"]["item"];
            let allowed = item["status"] == "completed";
            let denied = item["status"] == "declined";
            ensure!(
                allowed ^ denied,
                "native outcome was neither allow nor deny"
            );
            ensure!(
                response_kind != "allow" || allowed,
                "explicit allow was not applied"
            );
            ensure!(
                response_kind != "deny" || denied,
                "explicit deny was not applied"
            );
            if mode == 1 && allowed {
                ensure!(
                    item["exitCode"] == 0
                        && item["aggregatedOutput"]
                            .as_str()
                            .is_some_and(|s| s.contains("SYNTHETIC_RACE_COMMAND")),
                    "allowed command result mismatch"
                );
            }
            if mode == 2 {
                ensure!(
                    file.exists() == allowed,
                    "patch side effect differs from native decision"
                );
                if allowed {
                    ensure!(
                        std::fs::read(&file)? == b"SYNTHETIC_PATCH\n",
                        "synthetic patch content mismatch"
                    );
                }
            }
            passed(
                checks,
                case,
                "One exact native request resolved once and produced one terminal tool result consistent with execution or denial; a stale response did not reverse it. No client winner ACK is inferred.",
            );
        } else {
            let answers = model.answers.lock().unwrap();
            ensure!(
                answers.len() == 1,
                "expected one exact question tool output at synthetic model boundary"
            );
            let value = &answers[0];
            match response_kind {
                "race" => ensure!(
                    value == &answer("Yes") || value == &answer("No"),
                    "racing question answers merged or changed"
                ),
                "stale" => ensure!(
                    value == &answer("Yes"),
                    "stale response replaced current valid answer"
                ),
                "malformed" => ensure!(
                    value == &json!({"answers":{}}),
                    "malformed first answer did not consume as empty native answer"
                ),
                "unknown" => ensure!(
                    value
                        == &json!({"answers":{"unknown_synthetic_question":{"answers":["SYNTHETIC_UNKNOWN"]}}}),
                    "unknown question answer boundary differs"
                ),
                _ => unreachable!(),
            }
            passed(
                checks,
                case,
                match response_kind {
                    "malformed" => {
                        "An invalid answer shape consumed the raw callback and reached the model as empty answers; a later valid shape could not repair the request."
                    }
                    "unknown" => {
                        "A structurally valid answer for an unknown question ID passed through natively. Production must validate question identities before sending."
                    }
                    "stale" => {
                        "A response for a different request ID did not consume the blocking request; the valid exact-ID answer reached the model once."
                    }
                    _ => {
                        "Different answers were sent concurrently on independent native connections; exactly one unmerged answer reached the model and the stale answer did not replace it."
                    }
                },
            );
        }
    }
    Ok(())
}
