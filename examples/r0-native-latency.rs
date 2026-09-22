//! Fixed synthetic SSE -> ordinary installed CLI -> PTY comparisons.
//! PTY output includes native buffering/rendering; it is not a CLI socket or
//! physical-paint timestamp. Only aggregate statistics leave this experiment.

use std::collections::BTreeMap;
use std::io::{BufRead, Read};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
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
use serde_json::{Value, json};

#[path = "../src/workbench/test_native.rs"]
mod test_native;
use test_native::NativeProbe;

const PROMPT: &str = "请输出合成流式测量内容，不运行工具。";
const FINAL: &str = "R0_NATIVE_TIMING_DONE";

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "all", value_parser = ["all", "small", "long", "observer-paused"])]
    case: String,
    /// Two rounds alternate direct/proxy order to expose startup/scheduling noise.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=10))]
    rounds: u8,
}

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    samples: usize,
    repeats: usize,
    paused: bool,
}
const CASES: &[Case] = &[
    Case {
        name: "small",
        samples: 160,
        repeats: 4,
        paused: false,
    },
    Case {
        name: "long",
        samples: 120,
        repeats: 120,
        paused: false,
    },
    Case {
        name: "observer-paused",
        samples: 120,
        repeats: 24,
        paused: true,
    },
];

fn delta(case: Case, id: usize) -> String {
    format!("s{id:05} {}\n", "月光书架 ".repeat(case.repeats))
}
fn full_text(case: Case) -> String {
    (0..case.samples)
        .map(|id| delta(case, id))
        .collect::<String>()
        + FINAL
}
fn event(value: Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}
fn title_response(body: &Value) -> Option<Response> {
    body.pointer("/text/format/schema/properties/title")?;
    let output = json!({"type":"message","role":"assistant","id":"msg_title","content":[{"type":"output_text","text":"{\"title\":\"合成流测量\"}"}]});
    let bytes = [
        event(json!({"type":"response.created","response":{"id":"resp_title"}})),
        event(json!({"type":"response.output_item.done","output_index":0,"item":output})),
        event(json!({"type":"response.completed","response":{"id":"resp_title"}})),
    ]
    .concat();
    Some(
        Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(bytes))
            .unwrap(),
    )
}

type Times = Arc<Mutex<BTreeMap<usize, Instant>>>;
struct Fixture {
    base: String,
    emitted: Times,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn fixture(case: Case) -> Result<Fixture> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let emitted = Times::default();
    let times = emitted.clone();
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let app=Router::new().fallback(move |request:Request| {
        let times=times.clone(); let count=count.clone();
        async move {
            assert_eq!(request.headers().get(header::AUTHORIZATION).unwrap(), "Bearer synthetic-native-latency");
            let body:Value=serde_json::from_slice(&axum::body::to_bytes(request.into_body(),2*1024*1024).await.unwrap()).unwrap();
            if let Some(response)=title_response(&body) { return response; }
            assert_eq!(count.fetch_add(1,Ordering::SeqCst),0,"unexpected extra main model request");
            assert_eq!(body["model"],"gpt-6-astra");
            assert!(body["input"].to_string().contains(PROMPT));
            let stream=async_stream::stream! {
                yield Ok::<_,std::io::Error>(event(json!({"type":"response.created","response":{"id":"resp_native_timing"}})));
                yield Ok(event(json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant","id":"msg_timing","content":[]}})));
                let mut timer=tokio::time::interval(Duration::from_millis(10));
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                for id in 0..case.samples {
                    timer.tick().await;
                    let bytes=event(json!({"type":"response.output_text.delta","item_id":"msg_timing","content_index":0,"delta":delta(case,id)}));
                    times.lock().unwrap().insert(id,Instant::now());
                    yield Ok(bytes);
                }
                yield Ok(event(json!({"type":"response.output_text.delta","item_id":"msg_timing","content_index":0,"delta":FINAL})));
                yield Ok(event(json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","role":"assistant","id":"msg_timing","content":[{"type":"output_text","text":full_text(case)}]}})));
                yield Ok(event(json!({"type":"response.completed","response":{"id":"resp_native_timing"}})));
            };
            Response::builder().header(header::CONTENT_TYPE,"text/event-stream").body(Body::from_stream(stream)).unwrap()
        }
    });
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(Fixture {
        base,
        emitted,
        requests,
        task,
    })
}

fn native_completed(home: &Path, expected: &str) -> bool {
    walkdir::WalkDir::new(home.join("sessions"))
        .max_depth(5)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "jsonl")
        })
        .take(8)
        .any(|entry| {
            let Ok(file) = std::fs::File::open(entry.path()) else {
                return false;
            };
            std::io::BufReader::new(file.take(8 * 1024 * 1024))
                .lines()
                .map_while(std::result::Result::ok)
                .any(|line| {
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        return false;
                    };
                    value["type"] == "event_msg"
                        && matches!(
                            value["payload"]["type"].as_str(),
                            Some("task_complete" | "turn_complete")
                        )
                        && value["payload"]["last_agent_message"] == expected
                })
        })
}

fn sample_ids(screen: &str) -> impl Iterator<Item = usize> + '_ {
    screen.split_whitespace().filter_map(|word| {
        let digits = word.strip_prefix('s')?;
        (digits.len() == 5 && digits.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| digits.parse().ok())
            .flatten()
    })
}
fn distribution(values: &[f64]) -> Value {
    if values.is_empty() {
        return json!({"n":0});
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let q = |fraction: f64| {
        sorted[((fraction * sorted.len() as f64).ceil() as usize).saturating_sub(1)]
    };
    json!({"n":sorted.len(),"p50":q(0.5),"p95":q(0.95),"p99":q(0.99),"max":sorted.last(),"min":sorted.first()})
}

async fn run(case: Case, round: u8, proxied: bool) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&workspace)?;
    let upstream = fixture(case).await?;
    let config = format!(
        r#"model="gpt-6-astra"
model_provider="custom"
check_for_update_on_startup=false
[model_providers.custom]
name="Synthetic native latency"
base_url={}
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-native-latency"
"#,
        toml::Value::String(upstream.base.clone())
    );
    std::fs::write(home.join("config.toml"), &config)?;
    let (capture, receiver) = capture::channel(
        if case.paused {
            32 * 1024
        } else {
            8 * 1024 * 1024
        },
        256,
    );
    let proxy = if proxied {
        Some(ProxyServer::bind(Upstream::parse(&upstream.base)?, capture.clone()).await?)
    } else {
        None
    };
    let args = proxy
        .as_ref()
        .map(|proxy| {
            vec![
                "-c".into(),
                format!(
                    "model_providers.custom.base_url={}",
                    toml::Value::String(proxy.child_base_url())
                ),
            ]
        })
        .unwrap_or_default();
    let mut native = NativeProbe::start(&home, &workspace, &args)?;
    native.ready().await?;
    let hub = LiveHub::new(LiveLimits::default());
    let pause_started = Instant::now();
    let observer = if proxied {
        Some(Observer::start_with_delay(
            receiver,
            hub.clone(),
            RedactionPolicy::new(vec!["synthetic-native-latency".into()])?,
            DecoderLimits::default(),
            Duration::from_millis(if case.paused { 5000 } else { 0 }),
        )?)
    } else {
        drop(receiver);
        None
    };
    let started = Instant::now();
    native.submit(PROMPT).await?;
    let submitted = Instant::now();
    let mut seen = BTreeMap::<usize, Instant>::new();
    let mut final_at = None;
    let mut checked = Instant::now();
    let expected = full_text(case);
    let completion = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(at) = native.pump().await? {
                let screen = native.screen();
                for id in sample_ids(&screen).filter(|&id| id < case.samples) {
                    seen.entry(id).or_insert(at);
                }
                if screen.contains(FINAL) {
                    final_at.get_or_insert(at);
                }
            }
            if checked.elapsed() >= Duration::from_millis(50) {
                checked = Instant::now();
                if final_at.is_some() && native_completed(&home, &expected) {
                    return Ok::<_, anyhow::Error>(Instant::now());
                }
            }
        }
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!("native timing completion deadline; terminal details suppressed")
    })??;
    let pause_completed_ms = completion.duration_since(pause_started).as_secs_f64() * 1000.0;
    native.quit().await?;
    ensure!(
        upstream.requests.load(Ordering::SeqCst) == 1,
        "unexpected main request count"
    );
    let current: toml::Value = toml::from_str(&std::fs::read_to_string(home.join("config.toml"))?)?;
    let original: toml::Value = toml::from_str(&config)?;
    for key in ["model", "model_provider", "model_providers"] {
        ensure!(
            current[key] == original[key],
            "native provider configuration changed"
        );
    }
    if proxied && case.paused {
        ensure!(
            pause_completed_ms < 5000.0,
            "native completion waited for observer recovery"
        );
        tokio::time::sleep(Duration::from_millis(5100).saturating_sub(pause_started.elapsed()))
            .await;
        ensure!(
            capture.stats().dropped_chunks > 0,
            "paused observer did not exercise bounded overflow"
        );
        ensure!(
            hub.snapshot().capture == "partial",
            "observer gap was hidden"
        );
    } else if proxied {
        ensure!(
            capture.stats().dropped_chunks == 0,
            "unexpected capture drop"
        );
        ensure!(
            hub.snapshot()
                .model_items()
                .iter()
                .any(|item| item.text == expected),
            "observed final text differs from native completion"
        );
    }
    let emitted = upstream.emitted.lock().unwrap();
    ensure!(emitted.len() == case.samples, "fixture stream incomplete");
    let delays = seen
        .iter()
        .filter_map(|(id, at)| at.checked_duration_since(emitted[id]))
        .map(|delay| delay.as_secs_f64() * 1000.0)
        .collect::<Vec<_>>();
    ensure!(!delays.is_empty(), "no PTY markers observed");
    let first = seen.values().min().unwrap();
    ensure!(
        *first < *emitted.last_key_value().unwrap().1,
        "native terminal did not show an intermediate sample"
    );
    ensure!(
        delays.len() == seen.len(),
        "invalid monotonic sample mapping"
    );
    println!(
        "{}",
        json!({"stage":"result","case":case.name,"round":round,"route":if proxied {"proxy"} else {"direct"},
        "scope":"ordinary installed CLI, synthetic SSE, native terminal PTY read timestamp; includes CLI rendering, not socket receipt or display paint",
        "traffic":{"samples":case.samples,"deltaBytes":delta(case,0).len(),"intervalMs":10},
        "ms":{"emissionToPtyMarker":distribution(&delays),"submitToFirstPtyMarker":first.duration_since(submitted).as_secs_f64()*1000.0,
        "submitToFinalPtyMarker":final_at.unwrap().duration_since(submitted).as_secs_f64()*1000.0,"submitToNativeDurableCompletion":completion.duration_since(submitted).as_secs_f64()*1000.0,"draftAndTurn":completion.duration_since(started).as_secs_f64()*1000.0},
        "ptyMarkersObserved":seen.len(),"unobservedPtyMarkers":case.samples-seen.len(),"nativeIntermediateVerified":true,"nativeFullTextVerified":true,"providerConfigPreserved":true,
        "observerPauseMs":if proxied && case.paused {5000} else {0},"nativeCompletionSincePauseStartMs":pause_completed_ms,"capture":if proxied {serde_json::to_value(capture.stats())?} else {Value::Null},
        "comparison":"alternating independent runs; percentile difference is not a per-chunk causal proxy cost"})
    );
    drop(observer);
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let args = Args::parse();
    for &case in CASES
        .iter()
        .filter(|case| args.case == "all" || args.case == case.name)
    {
        for round in 0..args.rounds {
            for proxied in if round % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                println!(
                    "{}",
                    json!({"stage":"starting","case":case.name,"round":round,"route":if proxied {"proxy"} else {"direct"}})
                );
                run(case, round, proxied).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timing_samples_ignore_nonmarkers_and_use_nearest_rank_quantiles() {
        assert_eq!(
            sample_ids("s00001 s00002 s3 s0000x noise s00003.").collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(distribution(&[9.0, 1.0, 5.0])["p95"], 9.0);
        assert_eq!(distribution(&[])["n"], 0);
    }
}
