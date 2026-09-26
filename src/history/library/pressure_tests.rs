//! Opt-in G2 experiments reuse the original P07 clocks and transport probes.
use super::*;
use crate::workbench::{probe_process, test_native};
use anyhow::{Context, ensure};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[allow(dead_code)]
#[path = "../../../examples/support/r0_native_timing.rs"]
mod native_timing;
#[allow(dead_code)]
#[path = "../../../examples/support/r0_web_timing.rs"]
mod web_timing;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Load {
    Idle,
    Index,
    LegacyLock,
    QueryFlood,
}
impl Load {
    fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Index => "index",
            Self::LegacyLock => "legacy-lock",
            Self::QueryFlood => "query-flood",
        }
    }
}
fn loads() -> Vec<Load> {
    let selected = std::env::var("WORKBENCH_PRESSURE_LOAD").unwrap_or("all".into());
    let all = [Load::Idle, Load::Index, Load::LegacyLock, Load::QueryFlood];
    assert!(selected == "all" || all.iter().any(|load| load.name() == selected));
    all.into_iter()
        .filter(|load| selected == "all" || load.name() == selected)
        .collect()
}
fn cases<'a>(all: &'a [&'a str]) -> Vec<&'a str> {
    let selected = std::env::var("WORKBENCH_PRESSURE_CASE").unwrap_or("all".into());
    assert!(selected == "all" || all.contains(&selected.as_str()));
    all.iter()
        .copied()
        .filter(|name| selected == "all" || *name == selected)
        .collect()
}
fn corpus(temp: &tempfile::TempDir, loads: &[Load]) -> PathBuf {
    if loads.contains(&Load::Index) {
        let count = std::env::var("WORKBENCH_PRESSURE_SESSIONS")
            .ok()
            .map(|v| v.parse().unwrap())
            .unwrap_or(10_000);
        let (home, manifest) = super::scale_tests::generate_fixture(temp.path(), count);
        println!("{}", json!({"stage":"pressure-corpus","manifest":manifest}));
        home
    } else {
        let home = temp.path().join("native");
        std::fs::create_dir(&home).unwrap();
        home
    }
}

#[derive(Default)]
struct FloodStats {
    batches: u64,
    replies: BTreeMap<String, u64>,
    legacy_attempts: u64,
    legacy_busy: u64,
}
// Test-only evidence that retries actually reach the reader during traffic.
// Weak registrations cannot retain a finished fixture or its library.
type AttemptWatchers = Mutex<BTreeMap<String, std::sync::Weak<Mutex<FloodStats>>>>;
static ATTEMPT_WATCHERS: std::sync::OnceLock<AttemptWatchers> = std::sync::OnceLock::new();
pub(super) fn record_scan_attempt(source: &Source, error: Option<&str>) {
    if source.id() != "locked-legacy" {
        return;
    }
    let Some(watchers) = ATTEMPT_WATCHERS.get() else {
        return;
    };
    let stats = watchers
        .lock()
        .unwrap()
        .get(&source.identity())
        .and_then(std::sync::Weak::upgrade);
    if let Some(stats) = stats {
        let mut stats = stats.lock().unwrap();
        stats.legacy_attempts += 1;
        if error == Some("source_busy") {
            stats.legacy_busy += 1;
        }
    }
}
struct Pressure {
    mode: Load,
    root: tempfile::TempDir,
    index_home: PathBuf,
    library: Option<HistoryLibrary>,
    legacy: Option<crate::history::tests::Fixture>,
    source_before: Option<Vec<u8>>,
    lock: Option<Connection>,
    flood: Option<tokio::task::JoinHandle<()>>,
    stats: Arc<Mutex<FloodStats>>,
    starting_entries: u64,
    legacy_identity: Option<String>,
}
impl Pressure {
    fn new(mode: Load, index_home: &std::path::Path) -> Self {
        Self {
            mode,
            root: tempfile::tempdir().unwrap(),
            index_home: index_home.into(),
            library: None,
            legacy: None,
            source_before: None,
            lock: None,
            flood: None,
            stats: Arc::new(Mutex::new(FloodStats::default())),
            starting_entries: 0,
            legacy_identity: None,
        }
    }
    async fn activate(&mut self) -> anyhow::Result<()> {
        if self.mode == Load::Idle {
            return Ok(());
        }
        let mut config = LibraryConfig::default();
        let data = self.root.path().join("history");
        let home = if self.mode == Load::Index {
            self.index_home.clone()
        } else {
            let home = self.root.path().join("native");
            std::fs::create_dir(&home)?;
            home
        };
        if self.mode == Load::LegacyLock {
            let fixture = crate::history::tests::Fixture::new(25);
            self.source_before = Some(std::fs::read(&fixture.path)?);
            config.sources.push(Source::Observer {
                id: "locked-legacy".into(),
                database: fixture.path.clone(),
                blob_directory: Some(fixture.blobs.clone()),
                native_home: None,
            });
            self.legacy = Some(fixture);
        }
        self.library = Some(HistoryLibrary::with_policy(
            home,
            data.clone(),
            Arc::new(move || Ok(config.clone())),
        )?);
        let handle = self.library.as_ref().unwrap().handle();
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let statuses = handle.statuses();
                let ready = if self.mode == Load::Index {
                    statuses.iter().any(|s| {
                        s.id == "default-native" && s.state == "indexing" && s.indexed_entries > 0
                    })
                } else {
                    !statuses.is_empty()
                        && statuses
                            .iter()
                            .all(|s| s.revision.is_some() && s.state != "indexing")
                };
                if ready {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("history load did not become active")?;
        if self.mode == Load::Index {
            self.starting_entries = handle
                .statuses()
                .iter()
                .find(|s| s.id == "default-native")
                .unwrap()
                .indexed_entries;
        } else {
            let path = if self.mode == Load::LegacyLock {
                self.legacy.as_ref().unwrap().path.clone()
            } else {
                data.join("library/catalog.sqlite")
            };
            let db = Connection::open(path)?;
            db.busy_timeout(Duration::from_secs(2))?;
            db.execute_batch("BEGIN EXCLUSIVE")?;
            self.lock = Some(db);
            if self.mode == Load::LegacyLock {
                handle.refresh().await.map_err(anyhow::Error::msg)?;
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !handle.statuses().iter().any(|s| {
                        s.id == "locked-legacy" && s.error.as_deref() == Some("source_busy")
                    }) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .context("legacy source lock was not observed")?;
                let identity = handle
                    .statuses()
                    .into_iter()
                    .find(|s| s.id == "locked-legacy")
                    .unwrap()
                    .identity;
                ATTEMPT_WATCHERS
                    .get_or_init(Default::default)
                    .lock()
                    .unwrap()
                    .insert(identity.clone(), Arc::downgrade(&self.stats));
                self.legacy_identity = Some(identity);
                self.flood = Some(tokio::spawn(async move {
                    loop {
                        let _ = handle.refresh().await;
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                }));
            } else {
                let stats = self.stats.clone();
                self.flood = Some(tokio::spawn(async move {
                    loop {
                        let results = futures_util::future::join_all(
                            (0..64).map(|_| handle.list(Query::default(), false)),
                        )
                        .await;
                        {
                            let mut stats = stats.lock().unwrap();
                            stats.batches += 1;
                            for result in results {
                                *stats
                                    .replies
                                    .entry(result.err().unwrap_or("ok").into())
                                    .or_default() += 1;
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }));
                tokio::time::timeout(Duration::from_secs(5), async {
                    while self.stats.lock().unwrap().batches == 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .context("query flood was not observed")?;
            }
        }
        Ok(())
    }
    async fn finish(&mut self) -> anyhow::Result<Value> {
        if let Some(task) = self.flood.take() {
            task.abort();
            let _ = task.await;
        }
        let statuses = self
            .library
            .as_ref()
            .map(|library| library.handle().statuses())
            .unwrap_or_default();
        if self.mode == Load::Index {
            let source = statuses.iter().find(|s| s.id == "default-native").unwrap();
            ensure!(
                source.state == "indexing" && source.indexed_entries > self.starting_entries,
                "index pressure must span the entire traffic measurement"
            );
        }
        let (batches, replies, legacy_attempts, legacy_busy) = {
            let stats = self.stats.lock().unwrap();
            (
                stats.batches,
                stats.replies.clone(),
                stats.legacy_attempts,
                stats.legacy_busy,
            )
        };
        if self.mode == Load::LegacyLock {
            ensure!(
                legacy_attempts >= 2 && legacy_busy == legacy_attempts,
                "locked legacy reads must recur during measured traffic"
            );
        }
        if self.mode == Load::QueryFlood {
            ensure!(
                batches >= 2
                    && replies.get("library_busy").copied().unwrap_or(0) > 0
                    && replies.get("cache_invalid").copied().unwrap_or(0) > 0,
                "query pressure did not saturate the live queue and SQLite reader"
            );
            ensure!(
                replies.keys().all(|k| matches!(
                    k.as_str(),
                    "library_busy" | "cache_invalid" | "library_timeout"
                )),
                "unexpected flood outcome"
            );
        }
        let stop = Instant::now();
        // Keep the lock held during shutdown to verify bounded worker cancellation.
        drop(self.library.take());
        let stop_ms = stop.elapsed().as_secs_f64() * 1000.0;
        ensure!(stop_ms < 2000.0, "history shutdown exceeded 2s");
        if let Some(db) = self.lock.take() {
            db.execute_batch("ROLLBACK")?;
        }
        if let Some(fixture) = &self.legacy {
            ensure!(
                std::fs::read(&fixture.path)? == *self.source_before.as_ref().unwrap(),
                "legacy source changed"
            );
        }
        Ok(
            json!({"load":self.mode.name(),"startingEntries":self.starting_entries,"sources":statuses,"queryBatches":batches,"queryReplies":replies,"legacyReadAttemptsDuringTraffic":legacy_attempts,"legacyBusyDuringTraffic":legacy_busy,"queueCapacity":16,
            "queuePeak":if self.mode == Load::QueryFlood { Some(16) } else { None },
            "queuePeakMethod":"inferred from Full on a live bounded sync_channel(16); not a production metrics counter",
            "stopWhileLockedMs":stop_ms}),
        )
    }
}
impl Drop for Pressure {
    fn drop(&mut self) {
        if let Some(identity) = &self.legacy_identity {
            ATTEMPT_WATCHERS
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .remove(identity);
        }
        if let Some(task) = self.flood.take() {
            task.abort();
        }
        // Stop workers/locks before TempDir fields remove their files on errors.
        drop(self.library.take());
        drop(self.lock.take());
    }
}

fn web_gates(report: &Value) -> anyhow::Result<()> {
    let expected = report["traffic"]["requests"].as_u64().unwrap()
        * report["traffic"]["samplesPerRequest"].as_u64().unwrap();
    ensure!(
        report["clientSamples"] == expected && report["observedTextSamples"] == expected,
        "all transfer and observation samples are required"
    );
    for name in [
        "proxyCaptureToClient",
        "captureToDom",
        "captureToNextMainFramePaint",
    ] {
        ensure!(
            report["ms"][name]["n"] == expected,
            "missing timing samples: {name}"
        );
    }
    ensure!(
        report["ms"]["proxyCaptureToClient"]["p95"]
            .as_f64()
            .unwrap()
            <= 5.0,
        "P07 forwarding p95 exceeds 5ms"
    );
    ensure!(
        report["ms"]["captureToNextMainFramePaint"]["p95"]
            .as_f64()
            .unwrap()
            <= 100.0,
        "P07 visible-page p95 exceeds 100ms"
    );
    ensure!(
        report["clock"]["offsetDriftMs"].as_f64().unwrap().abs() < 5.0,
        "clock drift too large"
    );
    ensure!(
        report["clock"]["startHalfRttMs"].as_f64().unwrap() < 5.0
            && report["clock"]["endHalfRttMs"].as_f64().unwrap() < 5.0,
        "clock uncertainty too large"
    );
    ensure!(
        report["recording"]["persistedThroughViewSeq"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "no durable recorder progress"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "G2 P07 same-process history pressure + installed Chrome; writes >2GiB synthetic source; run alone"]
async fn g2_p07_history_pressure_web_latency_matrix() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let loads = loads();
    let home = corpus(&temp, &loads);
    for load in loads {
        for case in cases(&["small", "long", "concurrent"]) {
            println!(
                "{}",
                json!({"stage":"g2-web-start","load":load.name(),"case":case})
            );
            let mut pressure = Pressure::new(load, &home);
            let result = web_timing::measure_with_history(case, pressure.activate()).await;
            let report = result?;
            let pressure = pressure.finish().await?;
            println!(
                "{}",
                json!({"stage":"g2-web-result","pressure":pressure,"measurement":report})
            );
            web_gates(&report)?;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "G2 installed official CLI direct/proxy under history pressure; isolated temp HOME and synthetic upstream; run alone"]
async fn g2_p07_history_pressure_native_cli_matrix() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let loads = loads();
    let home = corpus(&temp, &loads);
    for load in loads {
        for case in cases(&["small", "long", "observer-paused"]) {
            for round in 0..2 {
                for proxied in if round == 0 {
                    [false, true]
                } else {
                    [true, false]
                } {
                    println!(
                        "{}",
                        json!({"stage":"g2-native-start","load":load.name(),"case":case,"round":round,"proxied":proxied})
                    );
                    let mut pressure = Pressure::new(load, &home);
                    let result = native_timing::measure_with_history(
                        case,
                        round,
                        proxied,
                        pressure.activate(),
                    )
                    .await;
                    let report = result?;
                    let pressure = pressure.finish().await?;
                    println!(
                        "{}",
                        json!({"stage":"g2-native-result","pressure":pressure,"measurement":report})
                    );
                }
            }
        }
    }
    Ok(())
}
