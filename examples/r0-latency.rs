//! Explicit synthetic R0 measurements. No model, credentials or user workspace.
//! Reports scope/clock error rather than equating DOM commits with screen paint.

#[path = "../src/workbench/probe_process.rs"]
mod probe_process;
use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::header;
use axum::response::Response;
use clap::Parser;
use codex_local_observer::workbench::capture;
use codex_local_observer::workbench::decode::DecoderLimits;
use codex_local_observer::workbench::live::{LiveHub, LiveLimits};
use codex_local_observer::workbench::observe::Observer;
use codex_local_observer::workbench::proxy::{ProxyServer, Upstream};
use codex_local_observer::workbench::redaction::RedactionPolicy;
use codex_local_observer::workbench::web::ReadingServer;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdout, Command};

#[derive(Parser)]
struct Args {
    /// Run one fixed case or all. Browser uses installed Chrome only.
    #[arg(long, default_value = "all", value_parser = ["all", "small", "long", "concurrent", "observer-paused", "slow-page", "storage-error"])]
    case: String,
    /// Measure the current workbench page, PTY and asynchronous Recorder.
    #[arg(long)]
    product_page: bool,
}

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    count: usize,
    bytes: usize,
    interval_ms: u64,
    requests: usize,
    observer_delay_ms: u64,
    page_busy_ms: u64,
}
const CASES: &[Case] = &[
    Case {
        name: "storage-error",
        count: 400,
        bytes: 64,
        interval_ms: 10,
        requests: 1,
        observer_delay_ms: 0,
        page_busy_ms: 0,
    },
    Case {
        name: "small",
        count: 400,
        bytes: 64,
        interval_ms: 10,
        requests: 1,
        observer_delay_ms: 0,
        page_busy_ms: 0,
    },
    Case {
        name: "long",
        count: 400,
        bytes: 2048,
        interval_ms: 10,
        requests: 1,
        observer_delay_ms: 0,
        page_busy_ms: 0,
    },
    Case {
        name: "concurrent",
        count: 200,
        bytes: 128,
        interval_ms: 20,
        requests: 4,
        observer_delay_ms: 0,
        page_busy_ms: 0,
    },
    Case {
        name: "observer-paused",
        count: 256,
        bytes: 2048,
        interval_ms: 4,
        requests: 1,
        observer_delay_ms: 5000,
        page_busy_ms: 0,
    },
    Case {
        name: "slow-page",
        count: 300,
        bytes: 64,
        interval_ms: 10,
        requests: 1,
        observer_delay_ms: 0,
        page_busy_ms: 1000,
    },
];

fn distribution(samples: &[f64]) -> Value {
    if samples.is_empty() {
        return json!({"n":0});
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let quantile = |q: f64| sorted[((sorted.len() as f64 * q).ceil() as usize).saturating_sub(1)];
    json!({"n":sorted.len(),"p50":quantile(0.50),"p95":quantile(0.95),"p99":quantile(0.99),"max":sorted.last().unwrap(),"min":sorted[0]})
}

struct Browser {
    child: probe_process::ProbeProcess,
    lines: Lines<BufReader<ChildStdout>>,
}
impl Browser {
    async fn start(url: &str, product_page: bool) -> Result<(Self, Value)> {
        let mut child = probe_process::ProbeProcess::spawn(
            Command::new("node")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/web/e2e/r0-latency-probe.cjs"
                ))
                .env("WORKBENCH_PROBE_URL", url)
                .env(
                    "WORKBENCH_PROBE_PRODUCT_PAGE",
                    if product_page { "1" } else { "0" },
                )
                .env("DEBUG", "")
                .env("PWDEBUG", "")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )?;
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut browser = Self { child, lines };
        let ready = browser.next().await?;
        ensure!(
            ready["stage"] == "ready",
            "browser pairing failed; details suppressed"
        );
        Ok((browser, ready))
    }
    async fn next(&mut self) -> Result<Value> {
        let line = tokio::time::timeout(Duration::from_secs(30), self.lines.next_line())
            .await??
            .ok_or_else(|| anyhow::anyhow!("browser exited before measurement"))?;
        let value: Value = serde_json::from_str(&line)
            .map_err(|_| anyhow::anyhow!("invalid measurement status"))?;
        ensure!(
            value["stage"] != "failed",
            "browser measurement failed; details suppressed"
        );
        Ok(value)
    }
    async fn command(&mut self, value: Value) -> Result<Value> {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(format!("{value}\n").as_bytes())
            .await?;
        self.next().await
    }
    async fn clock(&mut self, origin: Instant) -> Result<Clock> {
        let mut best = Clock {
            offset_ms: 0.0,
            half_rtt_ms: f64::INFINITY,
        };
        for _ in 0..15 {
            let before = origin.elapsed().as_secs_f64() * 1000.0;
            let report = self.command(json!({"op":"clock"})).await?;
            let after = origin.elapsed().as_secs_f64() * 1000.0;
            let browser_ms = report["browserMs"]
                .as_f64()
                .ok_or_else(|| anyhow::anyhow!("missing browser clock"))?;
            let half_rtt_ms = (after - before) / 2.0;
            if half_rtt_ms < best.half_rtt_ms {
                best = Clock {
                    offset_ms: (before + after) / 2.0 - browser_ms,
                    half_rtt_ms,
                };
            }
        }
        Ok(best)
    }
}
impl Drop for Browser {
    fn drop(&mut self) {
        self.child.stdin.take();
    }
}
struct Clock {
    offset_ms: f64,
    half_rtt_ms: f64,
}

type Times = Arc<Mutex<BTreeMap<usize, Instant>>>;
struct Fixture {
    base: String,
    emitted: Times,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn event(value: Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}
async fn fixture(case: Case) -> Result<Fixture> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let emitted = Times::default();
    let times = emitted.clone();
    let app = Router::new().fallback(move |request: Request| {
        let times = times.clone();
        async move {
            let request_id = request.uri().path().rsplit('/').next().unwrap_or("0").parse::<usize>().unwrap_or(0);
            let stream = async_stream::stream! {
                let response_id = format!("resp_{request_id}");
                let item_id = format!("msg_{request_id}");
                yield Ok::<_, std::io::Error>(event(json!({"type":"response.created","response":{"id":response_id}})));
                let mut timer = tokio::time::interval(Duration::from_millis(case.interval_ms));
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut full = String::new();
                for index in 0..case.count {
                    timer.tick().await;
                    let id = request_id * 10000 + index;
                    let text = format!("s{id:05} {}\n", "月光书架 ".repeat(case.bytes / 13));
                    full.push_str(&text);
                    let bytes = event(json!({"type":"response.output_text.delta","item_id":item_id,"content_index":0,"delta":text,"probe_sample":id}));
                    times.lock().unwrap().insert(id, Instant::now());
                    yield Ok(bytes);
                }
                yield Ok(event(json!({"type":"response.output_text.done","item_id":item_id,"content_index":0,"text":full})));
                yield Ok(event(json!({"type":"response.completed","response":{"id":response_id}})));
            };
            Response::builder().header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(stream)).unwrap()
        }
    });
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(Fixture {
        base,
        emitted,
        task,
    })
}

async fn receive(base: &str, case: Case, product_page: bool) -> Result<BTreeMap<usize, Instant>> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(20))
        .build()?;
    let mut jobs = tokio::task::JoinSet::new();
    for request in 0..case.requests {
        let client = client.clone();
        let url = format!("{base}/{request}");
        jobs.spawn(async move {
            let request = if product_page {
                client.post(url).header(header::CONTENT_TYPE, "application/json").body(json!({"model":"fixture-model", "stream":true,"input":[],"client_metadata":{"x-codex-turn-metadata":json!({"thread_id":"00000000-0000-4000-8000-000000000005","turn_id":format!("r5-perf-{request}"),"request_kind":"turn","thread_source":"user"}).to_string()}}).to_string())
            } else { client.get(url) };
            let mut stream = request
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("synthetic client connection failed"))?
                .bytes_stream();
            let mut pending = Vec::new();
            let mut received = BTreeMap::new();
            while let Some(bytes) = stream.next().await {
                let at = Instant::now();
                pending.extend_from_slice(
                    &bytes.map_err(|_| anyhow::anyhow!("synthetic response interrupted"))?,
                );
                ensure!(pending.len() <= 2 * 1024 * 1024, "measurement frame limit");
                while let Some(end) = pending.windows(2).position(|part| part == b"\n\n") {
                    let value: Value = serde_json::from_slice(&pending[6..end])?;
                    if let Some(id) = value["probe_sample"].as_u64() {
                        received.insert(id as usize, at);
                    }
                    pending.drain(..end + 2);
                }
            }
            Ok::<_, anyhow::Error>(received)
        });
    }
    let mut received = BTreeMap::new();
    while let Some(result) = jobs.join_next().await {
        received.extend(result??);
    }
    ensure!(
        received.len() == case.count * case.requests,
        "synthetic client lost body samples"
    );
    Ok(received)
}

struct EventTime {
    seq: u64,
    sample_id: usize,
    captured: Instant,
    published: Instant,
}
fn process_usage() -> (f64, i64) {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage writes a valid rusage into this allocation on success.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return (0.0, 0);
    }
    let usage = unsafe { usage.assume_init() };
    let cpu = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1_000_000.0;
    #[cfg(target_os = "macos")]
    let rss = usage.ru_maxrss;
    #[cfg(not(target_os = "macos"))]
    let rss = usage.ru_maxrss * 1024;
    (cpu, rss)
}

async fn run(case: Case, product_page: bool) -> Result<()> {
    let origin = Instant::now();
    let upstream = fixture(case).await?;
    let direct = receive(&upstream.base, case, product_page).await?;
    let direct_ms: Vec<_> = {
        let emitted = upstream.emitted.lock().unwrap();
        direct
            .iter()
            .map(|(id, at)| at.duration_since(emitted[id]).as_secs_f64() * 1000.0)
            .collect()
    };
    let (capture, receiver) = capture::channel(
        if case.observer_delay_ms > 0 {
            32 * 1024
        } else {
            8 * 1024 * 1024
        },
        256,
    );
    let proxy = ProxyServer::bind(Upstream::parse(&upstream.base)?, capture.clone()).await?;
    let hub = LiveHub::new(LiveLimits::default());
    let directory = tempfile::tempdir()?;
    let (_terminal, _recorder, web) = if product_page {
        use codex_local_observer::workbench::{
            recording::{Recorder, RecorderOptions},
            terminal::TerminalHost,
        };
        let root = directory.path().join("history");
        if case.name == "storage-error" {
            std::fs::write(&root, "synthetic non-directory storage fault")?;
        }
        let recorder = Recorder::start(
            &hub,
            RecorderOptions::new(root, directory.path(), "R5 performance fixture".into()),
        )?;
        let mut command = portable_pty::CommandBuilder::new("/bin/cat");
        command.cwd(directory.path());
        command.env("HOME", directory.path());
        command.env("USERPROFILE", directory.path());
        command.env("CODEX_HOME", directory.path());
        let terminal = TerminalHost::spawn(hub.epoch(), command, 45, 120)?;
        let web = ReadingServer::bind_with_terminal(hub.clone(), terminal.handle()).await?;
        (Some(terminal), Some(recorder), web)
    } else {
        (None, None, ReadingServer::bind(hub.clone()).await?)
    };
    let (mut browser, browser_info) = Browser::start(&web.bootstrap_url(), product_page).await?;
    let clock_start = browser.clock(origin).await?;
    let mut subscription = hub
        .subscribe(hub.epoch(), 0)
        .map_err(|_| anyhow::anyhow!("measurement subscription failed"))?;
    let events = Arc::new(Mutex::new(Vec::<EventTime>::new()));
    let observed = events.clone();
    let event_task = tokio::spawn(async move {
        while let Some(message) = subscription.recv().await {
            let published = Instant::now();
            let Ok(value) = serde_json::from_str::<Value>(&message.json) else {
                continue;
            };
            let text = if value["kind"] == "item.patch" && value["field"] == "text" {
                value["append"].as_str()
            } else if value["kind"] == "item.replace"
                && value["item"]["kind"] == "message"
                && value["item"]["revision"] == 1
            {
                value["item"]["content"][0]["text"].as_str()
            } else {
                None
            };
            let Some(id) = text
                .and_then(|text| text.strip_prefix('s'))
                .and_then(|text| text.split_whitespace().next())
                .and_then(|value| value.parse().ok())
            else {
                continue;
            };
            observed.lock().unwrap().push(EventTime {
                seq: message.sequence,
                sample_id: id,
                captured: message.received_at,
                published,
            });
        }
    });
    let _observer = Observer::start_with_delay(
        receiver,
        hub.clone(),
        RedactionPolicy::new(vec![])?,
        DecoderLimits::default(),
        Duration::from_millis(case.observer_delay_ms),
    )?;
    let before_usage = process_usage();
    ensure!(
        browser
            .command(json!({"op":"start","busyMs":case.page_busy_ms}))
            .await?["stage"]
            == "started",
        "browser start failed"
    );
    let start = Instant::now();
    let proxied = receive(&proxy.child_base_url(), case, product_page).await?;
    let forwarding_elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    if case.observer_delay_ms > 0 {
        ensure!(
            forwarding_elapsed_ms < case.observer_delay_ms as f64,
            "forwarding waited for paused observer"
        );
        ensure!(
            capture.stats().dropped_chunks > 0,
            "fault did not fill observation budget"
        );
        tokio::time::sleep(
            Duration::from_millis(case.observer_delay_ms).saturating_sub(start.elapsed())
                + Duration::from_millis(150),
        )
        .await;
    } else {
        tokio::time::timeout(Duration::from_secs(3), async {
            while events.lock().unwrap().len() < case.count * case.requests {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("observer did not publish every synthetic text delta"))?;
        ensure!(
            capture.stats().dropped_chunks == 0,
            "unexpected observation gap"
        );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let clock_end = browser.clock(origin).await?;
    let result = browser.command(json!({"op":"finish"})).await?;
    let usage = process_usage();
    event_task.abort();
    let events = events.lock().unwrap();
    let samples = result["samples"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing browser samples"))?;
    let mut dom_ms = Vec::new();
    let mut paint_ms = Vec::new();
    let mut pipeline_ms = Vec::new();
    let mut capture_client_ms = Vec::new();
    let mut dispatch_ms = Vec::new();
    for event in events.iter() {
        pipeline_ms.push(event.published.duration_since(event.captured).as_secs_f64() * 1000.0);
        if let Some(client_at) = proxied.get(&event.sample_id) {
            capture_client_ms.push(
                client_at
                    .checked_duration_since(event.captured)
                    .map_or(0.0, |d| d.as_secs_f64() * 1000.0),
            );
        }
        if let Some(received) = result["received"]
            .as_array()
            .and_then(|samples| samples.iter().find(|v| v["seq"] == event.seq))
        {
            dispatch_ms.push(
                received["at"].as_f64().unwrap() + clock_start.offset_ms
                    - event.captured.duration_since(origin).as_secs_f64() * 1000.0,
            );
        }
        if let Some(sample) = samples.iter().find(|sample| {
            if product_page {
                sample["sampleIds"].as_array().is_some_and(|ids| {
                    ids.iter()
                        .any(|id| id.as_u64() == Some(event.sample_id as u64))
                })
            } else {
                sample["seq"].as_u64().is_some_and(|seq| seq >= event.seq)
            }
        }) {
            let captured_ms = event.captured.duration_since(origin).as_secs_f64() * 1000.0;
            let dom = sample["domMs"].as_f64().unwrap() + clock_start.offset_ms - captured_ms;
            dom_ms.push(dom);
            if let Some(delay) = sample["paintDelayMs"].as_f64() {
                paint_ms.push(dom + delay);
            }
        }
    }
    let emitted = upstream.emitted.lock().unwrap();
    let proxied_ms: Vec<_> = proxied
        .iter()
        .map(|(id, at)| at.duration_since(emitted[id]).as_secs_f64() * 1000.0)
        .collect();
    ensure!(
        result["pageErrors"] == 0 && result["visibility"] == "visible",
        "page was not a healthy visible tab"
    );
    let report = json!({
        "stage":"result","case":case.name,"productPage":product_page,"scope":if product_page {"loopback HTTP client; current workbench page + /bin/cat PTY + Recorder; no official CLI/model in this benchmark"} else {"loopback HTTP client; lightweight reading page; no native CLI or model in this benchmark"},
        "traffic":{"requests":case.requests,"samplesPerRequest":case.count,"approxDeltaBytes":case.bytes,"intervalMs":case.interval_ms},
        "fault":{"observerPausedMs":case.observer_delay_ms,"pageBusyMs":case.page_busy_ms},
        "browser":browser_info,"clientSamples":proxied.len(),"observedTextSamples":events.len(),"domCommits":samples.len(),
        "ms":{"directEmissionToClient":distribution(&direct_ms),"proxyEmissionToClient":distribution(&proxied_ms),"proxyCaptureToClient":distribution(&capture_client_ms),"captureToObserverSubscriber":distribution(&pipeline_ms),"captureToBrowserEventDispatch":distribution(&dispatch_ms),"captureToDom":distribution(&dom_ms),"captureToNextMainFramePaint":distribution(&paint_ms)},
        "recording":hub.recorder_status(),
        "clock":{"method":"best of 15 midpoint RTT samples before and after traffic","startHalfRttMs":clock_start.half_rtt_ms,"endHalfRttMs":clock_end.half_rtt_ms,"offsetDriftMs":clock_end.offset_ms-clock_start.offset_ms},
        "forwardingElapsedMs":forwarding_elapsed_ms,"capture":capture.stats(),"browserMetrics":{"paintCount":result["paintCount"],"markCount":result["markCount"],"visibility":result["visibility"],"focused":result["focused"],"pageErrors":result["pageErrors"],"snapshots":result["snapshots"],"peakRssKiB":result["peakRssKiB"],"peakHeapBytes":result["peakHeapBytes"],"resourcesAvailable":result["resourcesAvailable"],"cpuSeconds":result["cpuSeconds"]},
        "rustHarness":{"scope":"fixture + proxy + observer + web + measurement; max RSS is process lifetime","peakRssBytes":usage.1,"cpuSeconds":usage.0-before_usage.0},
        "paintScope":"first trace Paint in the main frame after the DOM commit; not physical display presentation or proof every offscreen item was painted"
    });
    println!("{report}");
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let args = Args::parse();
    for &case in CASES.iter().filter(|case| {
        (args.case == "all" || args.case == case.name)
            && (args.product_page || case.name != "storage-error")
    }) {
        println!("{}", json!({"stage":"starting","case":case.name}));
        run(case, args.product_page).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_rank_percentiles_do_not_hide_empty_or_tail_samples() {
        assert_eq!(distribution(&[]), json!({"n":0}));
        let samples: Vec<_> = (1..=100).rev().map(f64::from).collect();
        let actual = distribution(&samples);
        assert_eq!(actual["p50"], 50.0);
        assert_eq!(actual["p95"], 95.0);
        assert_eq!(actual["p99"], 99.0);
        assert_eq!(actual["max"], 100.0);
    }
}
