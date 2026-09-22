//! Explicit R0 experiment, not the product launcher. Default mode only inspects
//! non-secret profile properties. `--live` runs a synthetic task against the
//! user's configured provider in a temporary workspace and private CODEX_HOME.

use std::io::{BufRead, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, bail, ensure};
use clap::Parser;
use codex_local_observer::workbench::capture;
use codex_local_observer::workbench::decode::{DecoderLimits, ResponseStatus};
use codex_local_observer::workbench::live::{LiveHub, LiveLimits};
use codex_local_observer::workbench::observe::Observer;
use codex_local_observer::workbench::proxy::{ProxyServer, Upstream};
use codex_local_observer::workbench::redaction::RedactionPolicy;
use codex_local_observer::workbench::web::ReadingServer;
use portable_pty::{Child, CommandBuilder, PtySize, native_pty_system};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use uuid::Uuid;

#[path = "r5/live_product.rs"]
mod live_product;
#[path = "../src/workbench/probe_process.rs"]
mod probe_process;
#[path = "../src/workbench/test_browser.rs"]
mod test_browser;

#[derive(Parser)]
struct Args {
    /// Opt in to sending this probe's synthetic prompt to the configured model.
    #[arg(long)]
    live: bool,
    /// Also verify one real read-only tool, a second-turn Esc and recovery.
    #[arg(long, requires = "live")]
    interactions: bool,
    /// Exercise the built product and native browser input, with a real image.
    #[arg(long, requires = "live", conflicts_with = "interactions")]
    product: bool,
    /// Include real-model requests for native approval and a Plan-mode question.
    #[arg(long, requires = "product")]
    product_controls: bool,
    /// Read only this configuration file; it is never used as a write target.
    #[arg(long)]
    config: Option<PathBuf>,
}

struct Profile {
    source: PathBuf,
    original: Vec<u8>,
    config: toml::Value,
    provider: String,
    model: String,
    upstream: String,
    secrets: Vec<String>,
    websocket_setting: Option<bool>,
    config_sources: codex_local_observer::workbench::config_sources::ConfigSources,
}
fn safe_identifier(value: Option<&str>) -> Result<String> {
    let value = value.ok_or_else(|| anyhow::anyhow!("missing profile identifier"))?;
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
        "unsupported profile identifier"
    );
    Ok(value.into())
}
impl Profile {
    fn read(path: Option<PathBuf>) -> Result<Self> {
        let source = match path {
            Some(path) => path,
            None => std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
                .ok_or_else(|| anyhow::anyhow!("no configured home"))?
                .join("config.toml"),
        };
        let original =
            std::fs::read(&source).map_err(|_| anyhow::anyhow!("cannot read source config"))?;
        let raw = std::str::from_utf8(&original)
            .map_err(|_| anyhow::anyhow!("source config is not UTF-8"))?;
        let value: toml::Value = toml::from_str(raw)
            .map_err(|_| anyhow::anyhow!("invalid source TOML; details suppressed"))?;
        let home = source
            .parent()
            .ok_or_else(|| anyhow::anyhow!("configuration file has no parent"))?;
        let config_sources =
            codex_local_observer::workbench::config_sources::inspect(home, &value)?;
        config_sources.require_verified()?;
        ensure!(
            value.get("profile").is_none(),
            "named default profile needs a separate precedence experiment"
        );
        let provider = safe_identifier(value.get("model_provider").and_then(toml::Value::as_str))?;
        let model = safe_identifier(value.get("model").and_then(toml::Value::as_str))?;
        let provider_value = value
            .get("model_providers")
            .and_then(|providers| providers.get(&provider))
            .ok_or_else(|| anyhow::anyhow!("missing selected custom provider"))?;
        ensure!(
            provider_value.get("wire_api").and_then(toml::Value::as_str) == Some("responses"),
            "probe only supports the selected Responses profile"
        );
        ensure!(
            provider_value
                .get("requires_openai_auth")
                .and_then(toml::Value::as_bool)
                == Some(false),
            "this isolated probe requires the configured custom auth profile"
        );
        let secret = provider_value
            .get("experimental_bearer_token")
            .and_then(toml::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("configured bearer profile required; auth is never substituted")
            })?
            .to_owned();
        ensure!(
            provider_value.get("env_key").is_none()
                && provider_value.get("env_http_headers").is_none(),
            "ambiguous additional credential sources need explicit verification"
        );
        let upstream = provider_value
            .get("base_url")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("explicit upstream required"))?
            .to_owned();
        Upstream::parse(&upstream)?;
        let websocket_setting = provider_value
            .get("supports_websockets")
            .and_then(toml::Value::as_bool);
        let mut secrets = vec![secret];
        if let Some(headers) = provider_value
            .get("http_headers")
            .and_then(toml::Value::as_table)
        {
            secrets.extend(
                headers
                    .values()
                    .filter_map(toml::Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
            );
        }
        let mut config = toml::Table::new();
        for key in [
            "model",
            "model_provider",
            "model_reasoning_effort",
            "model_reasoning_summary",
            "model_verbosity",
            "service_tier",
            "features",
        ] {
            if let Some(field) = value.get(key) {
                config.insert(key.into(), field.clone());
            }
        }
        config.insert(
            "model_providers".into(),
            toml::Value::Table(
                [(provider.clone(), provider_value.clone())]
                    .into_iter()
                    .collect(),
            ),
        );
        config.insert(
            "check_for_update_on_startup".into(),
            toml::Value::Boolean(false),
        );
        Ok(Self {
            source,
            original,
            config: toml::Value::Table(config),
            provider,
            model,
            upstream,
            secrets,
            websocket_setting,
            config_sources,
        })
    }
    fn public_description(&self) -> Value {
        let effort = match self
            .config
            .get("model_reasoning_effort")
            .and_then(toml::Value::as_str)
        {
            Some(
                value
                @ ("none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"),
            ) => value,
            Some(_) => "configured-other",
            None => "default",
        };
        json!({"provider":self.provider,"model":self.model,"reasoningEffort":effort,"wireApi":"responses","auth":"configured-custom-bearer","websocketsConfigured":self.websocket_setting,"configurationSources":self.config_sources,"scope":"isolated-synthetic-task"})
    }
    fn source_unchanged(&self) -> bool {
        std::fs::read(&self.source).is_ok_and(|bytes| bytes == self.original)
    }
}

struct NativeChild(Box<dyn Child + Send + Sync>);
impl Drop for NativeChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn terminal_queries(
    bytes: &[u8],
    terminal: &mut vt100::Parser,
    pending: &mut Vec<u8>,
    writer: &mut dyn Write,
) -> Result<()> {
    for &byte in bytes {
        terminal.process(&[byte]);
        if byte == 0x1b {
            pending.clear();
            pending.push(byte);
        } else if !pending.is_empty() {
            pending.push(byte);
            if pending.len() >= 3 && (0x40..=0x7e).contains(&byte) {
                if pending.as_slice() == b"\x1b[6n" {
                    let (row, column) = terminal.screen().cursor_position();
                    write!(writer, "\x1b[{};{}R", row + 1, column + 1)?;
                    writer.flush()?;
                }
                let reply = match pending.as_slice() {
                    b"\x1b[c" | b"\x1b[0c" => Some(b"\x1b[?1;2c".as_slice()),
                    b"\x1b[>c" | b"\x1b[>0c" => Some(b"\x1b[>0;0;0c".as_slice()),
                    b"\x1b[?u" => Some(b"\x1b[?0u".as_slice()),
                    _ => None,
                };
                if let Some(reply) = reply {
                    writer.write_all(reply)?;
                    writer.flush()?;
                }
                pending.clear();
            } else if pending.len() > 32 {
                pending.clear();
            }
        }
    }
    Ok(())
}

const MARKER: &str = "R0_REAL_READING_START";
const PROMPT: &str = "这是一个隔离的实时阅读测试。请不要调用任何工具、不要读取文件。回答第一行必须原样写 R0_REAL_READING_START，不加引号或代码块，然后输出一份关于虚构月球图书馆的中文导览：共 35 行，每行 25 至 40 个汉字，描述不同的书架、灯光或阅读体验。不要解释测试。";
const TOOL_PROMPT: &str = "这是隔离测试。请只调用一次终端工具执行只读命令 printf 'R0_NATIVE_TOOL_RESULT_6209\\n'，不要读文件或调用其它工具。命令成功后，回答第一行写 R0_REAL_READING_START，再用不少于 300 个汉字介绍虚构月球图书馆的书架、灯光与阅读体验。不要解释测试。";
const CANCEL_MARKER: &str = "R0_REAL_CANCEL_START";
const CANCEL_PROMPT: &str = "继续这个隔离测试，不调用工具、不读文件。第一行写 R0_REAL_CANCEL_START，然后连续输出 350 行关于虚构月球图书馆的中文阅读建议，每行约 30 字，不要提前总结。";
const AFTER_MARKER: &str = "R0_REAL_AFTER_CANCEL_START";
const AFTER_PROMPT: &str = "取消上一条长列表。现在不要调用工具、不读文件。第一行写 R0_REAL_AFTER_CANCEL_START，然后用不少于 300 个汉字描述虚构月球图书馆的宁静夜景。";

fn visible_tail_matches(text: &str, screen: &str) -> bool {
    // Native TUI renders Markdown; punctuation such as ** is not screen text.
    let letters = |text: &str| {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
    };
    let compact = letters(text);
    let tail: String = compact
        .chars()
        .rev()
        .take(60)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    tail.chars().count() >= 30
        && !letters(PROMPT).contains(&tail)
        && letters(screen).contains(&tail)
}

fn native_completed(home: &Path, text: &str) -> bool {
    // Only the newly created synthetic CODEX_HOME is scanned. This diagnostic
    // is independent of model completion and does not read private SQLite.
    walkdir::WalkDir::new(home.join("sessions"))
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .take(8)
        .any(|entry| {
            let Ok(file) = std::fs::File::open(entry.path()) else {
                return false;
            };
            std::io::BufReader::new(file.take(8 * 1024 * 1024))
                .lines()
                .map_while(Result::ok)
                .any(|line| {
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        return false;
                    };
                    value["type"] == "event_msg"
                        && matches!(
                            value["payload"]["type"].as_str(),
                            Some("task_complete" | "turn_complete")
                        )
                        && value["payload"]["error"].is_null()
                        && value["payload"]["last_agent_message"].as_str() == Some(text)
                })
        })
}

fn native_metadata(home: &Path) -> Value {
    let mut items = Vec::new();
    let mut completions = Vec::new();
    let mut errors = 0;
    let mut aborts = 0;
    for entry in walkdir::WalkDir::new(home.join("sessions"))
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "jsonl")
        })
        .take(8)
    {
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        for line in std::io::BufReader::new(file.take(8 * 1024 * 1024))
            .lines()
            .map_while(Result::ok)
        {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let payload = &value["payload"];
            if value["type"] == "event_msg" {
                match payload["type"].as_str() {
                    Some("error" | "stream_error") => errors += 1,
                    Some("turn_aborted") => aborts += 1,
                    _ => {}
                }
            }
            if value["type"] == "response_item" && items.len() < 64 {
                let kind = match payload["type"].as_str() {
                    Some("message") => "message",
                    Some("reasoning") => "reasoning",
                    Some("function_call") => "function_call",
                    Some("custom_tool_call") => "custom_tool_call",
                    _ => "other",
                };
                let tool = match payload["name"].as_str() {
                    Some("exec_command") => "exec_command",
                    Some("exec" | "functions.exec") => "exec",
                    Some("send_user_message_async") => "send_user_message_async",
                    Some("request_user_input") => "request_user_input",
                    _ => "other_or_none",
                };
                items.push(json!({"kind":kind,"tool":tool,"assistant":payload["role"] == "assistant","hasMarker":payload.to_string().contains(MARKER)}));
            }
            if value["type"] == "event_msg"
                && matches!(
                    payload["type"].as_str(),
                    Some("task_complete" | "turn_complete")
                )
            {
                let text = payload["last_agent_message"].as_str().unwrap_or("");
                completions.push(json!({"characters":text.chars().count(),"hasMarker":text.contains(MARKER),"error":!payload["error"].is_null()}));
            }
        }
    }
    json!({"items":items,"completions":completions,"errors":errors,"aborts":aborts})
}

fn native_tool_succeeded(home: &Path) -> bool {
    let mut calls = std::collections::HashSet::new();
    for entry in walkdir::WalkDir::new(home.join("sessions"))
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "jsonl")
        })
        .take(8)
    {
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        for line in std::io::BufReader::new(file.take(8 * 1024 * 1024))
            .lines()
            .map_while(Result::ok)
        {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let payload = &value["payload"];
            // Code mode stores nested tool returns as a durable TurnItem even
            // when the outer wire response is custom_tool_call. ExecCommandEnd
            // is transient in this CLI baseline, so do not depend on it here.
            let item = &payload["item"];
            if value["type"] == "event_msg"
                && payload["type"] == "item_completed"
                && item["type"] == "FunctionCallOutput"
                && matches!(
                    item["name"].as_str(),
                    Some("exec_command" | "shell_command")
                )
                && item["id"].as_str().is_some_and(|id| !id.is_empty())
                && successful_tool_output(&item["output"])
            {
                return true;
            }
            if value["type"] != "response_item" {
                continue;
            }
            let Some(id) = payload["call_id"].as_str() else {
                continue;
            };
            if payload["type"] == "function_call"
                && matches!(
                    payload["name"].as_str(),
                    Some("exec_command" | "shell_command")
                )
                && payload["arguments"].as_str().is_some_and(|args| {
                    args.contains("printf") && args.contains("R0_NATIVE_TOOL_RESULT_6209")
                })
            {
                calls.insert(id.to_owned());
            }
            if payload["type"] == "custom_tool_call"
                && matches!(payload["name"].as_str(), Some("exec" | "functions.exec"))
                && payload["input"].as_str().is_some_and(|input| {
                    input.contains("tools.exec_command")
                        && input.contains("printf")
                        && input.contains("R0_NATIVE_TOOL_RESULT_6209")
                })
            {
                calls.insert(id.to_owned());
            }
            if matches!(
                payload["type"].as_str(),
                Some("function_call_output" | "custom_tool_call_output")
            ) && calls.contains(id)
                && successful_tool_output(&payload["output"])
            {
                return true;
            }
        }
    }
    false
}

fn successful_tool_output(output: &Value) -> bool {
    let success = |text: &str| {
        if !text.contains("R0_NATIVE_TOOL_RESULT_6209") {
            return false;
        }
        if text.contains("Process exited with code 0") {
            return true;
        }
        serde_json::from_str::<Value>(text).is_ok_and(|result| {
            result["exit_code"] == 0
                && result["output"]
                    .as_str()
                    .is_some_and(|text| text.trim() == "R0_NATIVE_TOOL_RESULT_6209")
        })
    };
    output.as_str().is_some_and(success)
        || output.as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["type"] == "input_text" && item["text"].as_str().is_some_and(success)
            })
        })
}

async fn submit(writer: &mut dyn Write, prompt: &str) -> Result<()> {
    writer.write_all(format!("\x1b[200~{prompt}\x1b[201~").as_bytes())?;
    writer.flush()?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    writer.write_all(b"\r")?;
    writer.flush()?;
    Ok(())
}

struct NativeIo<'a> {
    terminal: &'a mut vt100::Parser,
    queries: &'a mut Vec<u8>,
    writer: &'a mut dyn Write,
    output: &'a mut mpsc::Receiver<Vec<u8>>,
}
impl NativeIo<'_> {
    fn apply(&mut self, bytes: Option<Vec<u8>>) -> Result<()> {
        let bytes =
            bytes.ok_or_else(|| anyhow::anyhow!("native CLI ended during interaction probe"))?;
        terminal_queries(&bytes, self.terminal, self.queries, self.writer)
    }
}

async fn interactions(
    home: &Path,
    hub: &std::sync::Arc<LiveHub>,
    capture: &capture::CaptureSender,
    web: &ReadingServer,
    mut native: NativeIo<'_>,
) -> Result<Value> {
    ensure!(
        native_tool_succeeded(home),
        "no matching successful native read-only tool output"
    );
    println!("{}", json!({"stage":"native-tool-verified"}));
    let baseline_aborts = native_metadata(home)["aborts"].as_u64().unwrap_or(0);
    let baseline_interrupted = capture.stats().interrupted_streams;
    submit(native.writer, CANCEL_PROMPT).await?;
    let started = Instant::now();
    let mut progress_at = started + Duration::from_secs(15);
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    let mut escaped = false;
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            tokio::select! {
                bytes = native.output.recv() => native.apply(bytes)?,
                _ = tick.tick() => {},
            }
            if !escaped {
                let snapshot = hub.snapshot();
                if snapshot.model_items().iter().any(|item| item.text.contains(CANCEL_MARKER)
                    && item.text.chars().count() >= 160
                    && snapshot.responses.iter().any(|response| response.request_id == item.key.request_id && response.status == ResponseStatus::Receiving)) {
                    native.writer.write_all(b"\x1b")?; native.writer.flush()?;
                    escaped = true;
                    println!("{}", json!({"stage":"native-escape-sent","elapsedMs":started.elapsed().as_millis()}));
                }
            } else if native_metadata(home)["aborts"].as_u64().unwrap_or(0) > baseline_aborts
                && capture.stats().interrupted_streams > baseline_interrupted {
                return Ok::<_, anyhow::Error>(());
            }
            if Instant::now() >= progress_at {
                println!("{}", json!({"stage":"waiting-for-native-cancel","elapsedMs":started.elapsed().as_millis(),"escaped":escaped}));
                progress_at = Instant::now() + Duration::from_secs(15);
            }
        }
    }).await.map_err(|_| anyhow::anyhow!("native cancellation verification deadline"))??;
    println!(
        "{}",
        json!({"stage":"native-cancel-verified","nativeAbort":true,"unfinishedStreamClosed":true})
    );
    // The next ordinary input stays in the same PTY and CLI process. Opening
    // another observer page only verifies resumed reading, not a new session.
    let mut browser =
        test_browser::BrowserProbe::start(&web.bootstrap_url(), AFTER_MARKER, "", "live", None)?;
    ensure!(
        browser.next().await?["stage"] == "ready",
        "recovery browser pairing failed"
    );
    submit(native.writer, AFTER_PROMPT).await?;
    let mut request_id = None;
    let mut browser_done = false;
    let (mut visible, mut complete) = (false, false);
    progress_at = Instant::now() + Duration::from_secs(15);
    tokio::time::timeout(Duration::from_secs(310), async {
        loop {
            tokio::select! {
                bytes = native.output.recv() => native.apply(bytes)?,
                report = browser.next(), if !browser_done => {
                    let report = report?;
                    match report["stage"].as_str().unwrap_or("") {
                        "intermediate" => {
                            let id = report["requestId"].as_str().and_then(|id| Uuid::parse_str(id).ok()).ok_or_else(|| anyhow::anyhow!("invalid recovery request identity"))?;
                            ensure!(hub.snapshot().responses.iter().any(|response| response.request_id == id && response.status == ResponseStatus::Receiving), "recovery intermediate came after completion");
                            request_id = Some(id);
                        },
                        "final" => {},
                        "complete" => browser_done = true,
                        _ => bail!("recovery browser verification failed; details suppressed"),
                    }
                },
                _ = tick.tick() => {},
            }
            if browser_done && let Some(id) = request_id {
                for item in hub.snapshot().model_items().iter().filter(|item| item.key.request_id == id && item.text.contains(AFTER_MARKER)) {
                    visible |= visible_tail_matches(&item.text, &native.terminal.screen().contents());
                    complete |= native_completed(home, &item.text);
                }
                if visible && complete { return Ok::<_, anyhow::Error>(()); }
            }
            if Instant::now() >= progress_at {
                println!("{}", json!({"stage":"waiting-for-recovery","elapsedMs":started.elapsed().as_millis(),"browserMidstream":request_id.is_some(),"nativeFinalVisible":visible,"nativeTurnCompleted":complete}));
                progress_at = Instant::now() + Duration::from_secs(15);
            }
        }
    }).await.map_err(|_| anyhow::anyhow!("native recovery verification deadline"))??;
    browser.wait().await?;
    Ok(
        json!({"toolSucceeded":true,"nativeAbort":true,"unfinishedStreamClosed":true,"sameCliRecovery":true,"recoveryBrowserMidstream":true,"recoveryNativeVisible":visible,"recoveryTurnCompleted":complete,"elapsedMs":started.elapsed().as_millis()}),
    )
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let args = Args::parse();
    let prompt = if args.interactions {
        TOOL_PROMPT
    } else {
        PROMPT
    };
    let profile = Profile::read(args.config)?;
    println!(
        "{}",
        json!({"stage":"profile-inspected","profile":profile.public_description(),"sourceUnchanged":profile.source_unchanged(),"live":args.live})
    );
    if !args.live {
        return Ok(());
    }
    if args.product {
        return live_product::run(&profile, args.product_controls).await;
    }
    let directory = tempfile::tempdir()?;
    let home = directory.path().join("codex-home");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&workspace)?;
    std::fs::write(
        workspace.join("README.md"),
        "# Isolated R0 live profile probe\nSynthetic task only.\n",
    )?;
    let config_text = toml::to_string(&profile.config)
        .map_err(|_| anyhow::anyhow!("cannot encode isolated config"))?;
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(home.join("config.toml"))?
        .write_all(config_text.as_bytes())?;
    let (capture, receiver) = capture::channel(8 * 1024 * 1024, 256);
    let proxy = ProxyServer::bind(Upstream::parse(&profile.upstream)?, capture.clone()).await?;
    let policy = RedactionPolicy::new(profile.secrets.clone())?;
    let hub = LiveHub::new(LiveLimits::default());
    let _observer = Observer::start(receiver, hub.clone(), policy, DecoderLimits::default())?;
    let web = ReadingServer::bind(hub.clone()).await?;
    let mut browser =
        test_browser::BrowserProbe::start(&web.bootstrap_url(), MARKER, "", "live", None)?;
    let ready = tokio::time::timeout(Duration::from_secs(20), browser.next()).await??;
    ensure!(ready["stage"] == "ready", "live browser could not start");

    let pair = native_pty_system().openpty(PtySize {
        rows: 45,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let mut command = CommandBuilder::new(
        std::env::var_os("WORKBENCH_TEST_CODEX").unwrap_or_else(|| "codex".into()),
    );
    command.cwd(&workspace);
    command.env("HOME", directory.path());
    command.env("USERPROFILE", directory.path());
    command.env("CODEX_HOME", &home);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.arg("-c");
    command.arg(format!(
        "model_providers.{}.base_url={}",
        profile.provider,
        toml::Value::String(proxy.child_base_url())
    ));
    let child = NativeChild(pair.slave.spawn_command(command)?);
    drop(pair.slave);
    let (output_tx, mut output_rx) = mpsc::channel(64);
    let reader_thread = std::thread::spawn(move || {
        let mut bytes = [0u8; 8192];
        while let Ok(size) = reader.read(&mut bytes) {
            if size == 0 || output_tx.blocking_send(bytes[..size].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut terminal = vt100::Parser::new(45, 120, 0);
    let mut queries = Vec::new();
    let (mut theme, mut trusted, mut submitted, mut browser_done, mut intermediate) =
        (false, false, false, false, false);
    let mut request_id = None;
    let (mut native_visible, mut native_done, mut final_characters) = (false, false, 0);
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    let started = Instant::now();
    let mut progress_at = started + Duration::from_secs(15);
    // Preserve the actual configured reasoning effort. A title often arrives
    // long before the main reply; a short DOM timeout must not end that reply.
    let mut outcome = tokio::time::timeout(Duration::from_secs(320), async {
        loop {
            tokio::select! {
                bytes = output_rx.recv() => {
                    let bytes = bytes.ok_or_else(|| anyhow::anyhow!("native CLI ended before verification"))?;
                    terminal_queries(&bytes, &mut terminal, &mut queries, writer.as_mut())?;
                    let screen = terminal.screen().contents();
                    if !theme && (screen.contains("Choose your style") || screen.contains("Select a theme")) {
                        writer.write_all(b"\r")?; writer.flush()?; theme = true;
                    } else if !trusted && (screen.contains("Do you trust") || screen.contains("Do you want to work in this directory")) {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        writer.write_all(b"\r")?; writer.flush()?; trusted = true;
                    } else if !submitted && trusted && screen.contains("OpenAI Codex") && screen.contains('›') {
                        submit(writer.as_mut(), prompt).await?; submitted = true;
                        println!("{}", json!({"stage":"native-input-submitted"}));
                    }
                }
                report = browser.next(), if !browser_done => {
                    let report = report?;
                    match report["stage"].as_str().unwrap_or("") {
                        "intermediate" => {
                            let id = report["requestId"].as_str().and_then(|id| Uuid::parse_str(id).ok()).ok_or_else(|| anyhow::anyhow!("invalid browser response identity"))?;
                            ensure!(hub.snapshot().responses.iter().any(|response| response.request_id == id && response.status == ResponseStatus::Receiving), "upstream completed before browser intermediate acknowledgement");
                            request_id = Some(id); intermediate = true;
                            println!("{}", json!({"stage":"browser-midstream","characters":report["characters"],"elapsedMs":started.elapsed().as_millis()}));
                        }
                        "final" => println!("{}", json!({"stage":"model-response-completed","elapsedMs":started.elapsed().as_millis()})),
                        "complete" => { browser_done = true; println!("{}", json!({"stage":"browser-verified","browser":report["browser"],"headless":report["headless"],"pageErrors":report["pageErrors"]})); }
                        _ => bail!("live browser verification failed; raw errors suppressed"),
                    }
                }
                _ = tick.tick() => {}
            }
            if Instant::now() >= progress_at {
                let metadata = native_metadata(&home);
                println!("{}", json!({"stage":"waiting-for-evidence","elapsedMs":started.elapsed().as_millis(),"nativeItemCount":metadata["items"].as_array().map(Vec::len),"nativeCompletions":metadata["completions"].as_array().map(Vec::len),"nativeErrors":metadata["errors"],"nativeAborts":metadata["aborts"],"browserMidstream":intermediate,"captureStats":capture.stats()}));
                progress_at = Instant::now() + Duration::from_secs(15);
            }
            if browser_done && let Some(id) = request_id {
                let snapshot = hub.snapshot();
                for item in snapshot.model_items().iter().filter(|item| item.key.request_id == id) {
                    if !item.text.contains(MARKER) || item.text.chars().count() < 160 { continue; }
                    final_characters = item.text.chars().count();
                    native_visible |= visible_tail_matches(&item.text, &terminal.screen().contents());
                    native_done |= native_completed(&home, &item.text);
                }
                if native_visible && native_done { return Ok::<_, anyhow::Error>(()); }
            }
        }
    }).await;
    let mut interaction_result = Value::Null;
    if matches!(outcome, Ok(Ok(()))) && args.interactions {
        match interactions(
            &home,
            &hub,
            &capture,
            &web,
            NativeIo {
                terminal: &mut terminal,
                queries: &mut queries,
                writer: writer.as_mut(),
                output: &mut output_rx,
            },
        )
        .await
        {
            Ok(result) => interaction_result = result,
            Err(error) => outcome = Ok(Err(error)),
        }
    }
    if !matches!(outcome, Ok(Ok(()))) {
        println!(
            "{}",
            json!({"stage":"failure-metadata","native":native_metadata(&home),"readingItems":hub.snapshot().model_items().iter().map(|item| json!({"characters":item.text.chars().count(),"containsMarker":item.text.contains(MARKER),"jsonTitle":serde_json::from_str::<Value>(&item.text).is_ok_and(|value|value.get("title").is_some())})).collect::<Vec<_>>()})
        );
    }
    // Cleanup before reporting any failure; never print the terminal or raw
    // model/proxy errors because they can contain provider details or routes.
    drop(child);
    drop(writer);
    drop(pair.master);
    output_rx.close();
    drop(output_rx);
    let _ = reader_thread.join();
    let config_after: toml::Value =
        toml::from_str(&std::fs::read_to_string(home.join("config.toml"))?)
            .map_err(|_| anyhow::anyhow!("cannot verify isolated configuration"))?;
    let unchanged = profile.source_unchanged()
        && config_after.get("model_providers") == profile.config.get("model_providers")
        && config_after.get("model") == profile.config.get("model")
        && config_after.get("model_provider") == profile.config.get("model_provider");
    let snapshot = hub.snapshot();
    println!(
        "{}",
        json!({"stage":"result","passed":matches!(outcome,Ok(Ok(()))),"interactions":interaction_result,"submitted":submitted,"browserMidstream":intermediate,"nativeFinalVisible":native_visible,"nativeTurnCompleted":native_done,"finalCharacters":final_characters,"sourceAndProviderUnchanged":unchanged,"capture":snapshot.capture,"diagnosticCodes":snapshot.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),"captureStats":capture.stats(),"elapsedMs":started.elapsed().as_millis()})
    );
    ensure!(unchanged, "profile configuration changed during probe");
    outcome.map_err(|_| anyhow::anyhow!("live profile probe deadline; payload suppressed"))??;
    ensure!(
        submitted && intermediate,
        "native/browser evidence incomplete"
    );
    browser.wait().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_probe_requires_a_matching_native_call_and_successful_return() {
        let directory = tempfile::tempdir().unwrap();
        let sessions = directory.path().join("sessions");
        std::fs::create_dir(&sessions).unwrap();
        let path = sessions.join("synthetic.jsonl");
        let call = json!({"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"fixture_call","arguments":"{\"cmd\":\"printf 'R0_NATIVE_TOOL_RESULT_6209\\\\n'\"}"}});
        let success = json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"fixture_call","output":"Process exited with code 0\nFinal output:\nR0_NATIVE_TOOL_RESULT_6209\n"}});
        std::fs::write(&path, format!("{call}\n")).unwrap();
        assert!(!native_tool_succeeded(directory.path()));
        // Shape observed from the installed CLI's synthetic Code Mode run.
        let custom_call = json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"custom_fixture","input":"text(await tools.exec_command({cmd: \"printf 'R0_NATIVE_TOOL_RESULT_6209\\\\n'\"}));"}});
        let custom_output = json!({"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"custom_fixture","output":[{"type":"input_text","text":"Script completed\nOutput:\n"},{"type":"input_text","text":json!({"exit_code":0,"output":"R0_NATIVE_TOOL_RESULT_6209\n"}).to_string()}]}});
        std::fs::write(&path, format!("{custom_call}\n{custom_output}\n")).unwrap();
        assert!(native_tool_succeeded(directory.path()));
        let mut wrong = success.clone();
        wrong["payload"]["call_id"] = json!("unrelated_call");
        std::fs::write(&path, format!("{call}\n{wrong}\n")).unwrap();
        assert!(!native_tool_succeeded(directory.path()));
        let mut failed = success.clone();
        failed["payload"]["output"] =
            json!("Process exited with code 1\nR0_NATIVE_TOOL_RESULT_6209");
        std::fs::write(&path, format!("{call}\n{failed}\n")).unwrap();
        assert!(!native_tool_succeeded(directory.path()));
        std::fs::write(&path, format!("{call}\n{success}\n")).unwrap();
        assert!(native_tool_succeeded(directory.path()));
        let nested = json!({"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FunctionCallOutput","id":"synthetic_nested_call","name":"exec_command","output":[{"type":"input_text","text":success["payload"]["output"]}]}}});
        std::fs::write(&path, format!("{nested}\n")).unwrap();
        assert!(native_tool_succeeded(directory.path()));
        let mut wrong = nested;
        wrong["payload"]["item"]["name"] = json!("unrelated_tool");
        std::fs::write(&path, format!("{wrong}\n")).unwrap();
        assert!(!native_tool_succeeded(directory.path()));
    }
    #[test]
    fn native_render_matching_accounts_for_markdown_and_never_accepts_input_echo() {
        let text = "在月球图书馆的静谧角落，**星光书架**铺开了一片蓝色光晕，让每位旅人找到可以安静阅读的长椅。";
        assert!(visible_tail_matches(text, &text.replace("**", "")));
        assert!(!visible_tail_matches(PROMPT, PROMPT));
        assert!(!visible_tail_matches(text, "输入区与加载提示"));
    }
    #[test]
    fn cursor_position_query_uses_the_current_emulated_position() {
        let mut output = Vec::new();
        terminal_queries(
            b"\x1b[12;34H\x1b[6n",
            &mut vt100::Parser::new(45, 120, 0),
            &mut Vec::new(),
            &mut output,
        )
        .unwrap();
        assert_eq!(output, b"\x1b[12;34R");
    }
}
