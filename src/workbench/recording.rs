//! Bounded, best-effort recording. Producers never wait for filesystem I/O.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use uuid::Uuid;

use super::decode::details::DetailDocument;
use super::live::{LiveHub, Published};
pub(crate) mod fs;
pub mod history;
mod journal;
mod lifecycle;
pub mod management;
pub(crate) mod replay;
use fs::Directory;
use journal::{Meta, Writer};
use replay::{Checkpoint, Document};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecorderStatus {
    pub run_epoch: Uuid,
    pub state: &'static str,
    pub persisted_through_view_seq: u64,
    pub saved_through_view_seq: u64,
    pub observed_view_seq: u64,
    pub history_coverage: &'static str,
    pub gap_count: usize,
    pub error: Option<&'static str>,
}
impl RecorderStatus {
    pub fn disabled(epoch: Uuid) -> Self {
        Self {
            run_epoch: epoch,
            state: "disabled",
            persisted_through_view_seq: 0,
            saved_through_view_seq: 0,
            observed_view_seq: 0,
            history_coverage: "partial",
            gap_count: 0,
            error: None,
        }
    }
}
pub struct RecorderOptions {
    pub root: PathBuf,
    // The default .codex-web parent may not exist when config is overridden.
    // Create it on the storage worker so failure still degrades only recording.
    pub(crate) private_parent: Option<PathBuf>,
    pub workspace_id: String,
    pub project_name: String,
    pub queue_bytes: usize,
    pub queue_events: usize,
    pub sync_interval: Duration,
    #[cfg(test)]
    pub faults: Arc<Faults>,
}
impl RecorderOptions {
    pub fn new(root: PathBuf, cwd: &std::path::Path, project_name: String) -> Self {
        Self {
            root,
            private_parent: None,
            workspace_id: blake3::hash(cwd.as_os_str().as_encoded_bytes())
                .to_hex()
                .to_string(),
            project_name,
            queue_bytes: 8 * 1024 * 1024,
            queue_events: 256,
            sync_interval: Duration::from_secs(1),
            #[cfg(test)]
            faults: Arc::default(),
        }
    }
}
#[cfg(test)]
#[derive(Default)]
pub struct Faults {
    pub pause_ms: AtomicU64,
    pub pause_active: AtomicBool,
    pub write_error: AtomicBool,
    pub sync_error: AtomicBool,
}
enum Content {
    View(Arc<Published>, Option<Checkpoint>),
    Document {
        request_id: Uuid,
        capture_seq: u64,
        document: DetailDocument,
    },
}
struct Packet {
    record_seq: u64,
    view_seq: u64,
    content: Content,
    _permit: OwnedSemaphorePermit,
}
struct Shared {
    next: AtomicU64,
    durable_record: AtomicU64,
    latest_view: AtomicU64,
    lost: AtomicBool,
    stop: AtomicBool,
    clean_exit: AtomicBool,
    status: watch::Sender<RecorderStatus>,
}
#[derive(Clone)]
pub(crate) struct Sink {
    sender: mpsc::SyncSender<Packet>,
    budget: Arc<Semaphore>,
    shared: Arc<Shared>,
}
impl Sink {
    fn send(&self, view_seq: u64, bytes: usize, content: Content) {
        let record_seq = self.shared.next.fetch_add(1, Ordering::AcqRel) + 1;
        self.shared.latest_view.store(view_seq, Ordering::Release);
        let permit = u32::try_from(bytes)
            .ok()
            .and_then(|bytes| self.budget.clone().try_acquire_many_owned(bytes).ok());
        let ok = permit.is_some_and(|permit| {
            self.sender
                .try_send(Packet {
                    record_seq,
                    view_seq,
                    content,
                    _permit: permit,
                })
                .is_ok()
        });
        if !ok {
            self.shared.lost.store(true, Ordering::Release);
        }
    }
    pub(crate) fn view(&self, value: Arc<Published>, checkpoint: Option<Checkpoint>) {
        let bytes = value.json.len()
            + checkpoint.as_ref().map_or(0, |c| {
                serde_json::to_vec(c).map_or(usize::MAX / 2, |b| b.len())
            });
        self.send(value.sequence, bytes, Content::View(value, checkpoint));
    }
    pub(crate) fn document(
        &self,
        sequence: u64,
        request_id: Uuid,
        capture_seq: u64,
        document: &DetailDocument,
    ) {
        self.send(
            sequence,
            document.bytes(),
            Content::Document {
                request_id,
                capture_seq,
                document: document.clone(),
            },
        );
    }
    pub(crate) fn sequence(&self) -> u64 {
        self.shared.next.load(Ordering::Acquire)
    }
    pub(crate) fn status(&self) -> RecorderStatus {
        let mut status = self.shared.status.borrow().clone();
        status.observed_view_seq = self.shared.latest_view.load(Ordering::Acquire);
        if self.shared.lost.load(Ordering::Acquire) {
            status.state = "degraded";
            status.error = Some("queue_full");
            status.history_coverage = "partial";
        } else if status.state == "saved"
            && self.shared.next.load(Ordering::Acquire)
                > self.shared.durable_record.load(Ordering::Acquire)
        {
            status.state = "pending";
        }
        status
    }
    pub(crate) fn subscribe(&self) -> watch::Receiver<RecorderStatus> {
        self.shared.status.subscribe()
    }
}

pub struct Recorder {
    shared: Arc<Shared>,
    done: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
    pub history: history::History,
}
impl Recorder {
    pub fn start(hub: &Arc<LiveHub>, options: RecorderOptions) -> anyhow::Result<Self> {
        anyhow::ensure!(
            options.queue_bytes > 0
                && options.queue_events > 0
                && options.queue_bytes <= u32::MAX as usize,
            "invalid recorder budget"
        );
        let (sender, receiver) = mpsc::sync_channel(options.queue_events);
        let mut initial = RecorderStatus::disabled(hub.epoch());
        initial.state = "pending";
        let (status, _) = watch::channel(initial);
        let shared = Arc::new(Shared {
            next: AtomicU64::new(0),
            durable_record: AtomicU64::new(0),
            latest_view: AtomicU64::new(0),
            lost: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            clean_exit: AtomicBool::new(true),
            status,
        });
        let sink = Sink {
            sender,
            budget: Arc::new(Semaphore::new(options.queue_bytes)),
            shared: shared.clone(),
        };
        let checkpoint = hub.attach_recorder(sink)?;
        let history = history::History::start(
            options.root.clone(),
            options.workspace_id.clone(),
            hub.epoch(),
        )?;
        hub.set_history(history.clone());
        let (finished, done) = mpsc::channel();
        let weak = Arc::downgrade(hub);
        let worker_shared = shared.clone();
        let epoch = hub.epoch();
        let thread = std::thread::Builder::new()
            .name("workbench-recorder".into())
            .spawn(move || {
                record(weak, epoch, receiver, worker_shared, options, checkpoint);
                let _ = finished.send(());
            })?;
        Ok(Self {
            shared,
            done,
            thread: Some(thread),
            history,
        })
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        // A filesystem can block indefinitely. Detach after the exit budget;
        // only a successful final commit may mark the run cleanly ended.
        if self.done.recv_timeout(Duration::from_secs(2)).is_ok() {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        } else {
            self.shared.clean_exit.store(false, Ordering::Release);
            self.shared.status.send_modify(|s| {
                s.state = "degraded";
                s.error = Some("shutdown_timeout");
            });
        }
    }
}
fn status(
    shared: &Shared,
    meta: Option<&Meta>,
    epoch: Uuid,
    error: Option<&'static str>,
    history_partial: bool,
) {
    shared
        .durable_record
        .store(meta.map_or(0, |m| m.saved_record_seq), Ordering::Release);
    let latest = shared.latest_view.load(Ordering::Acquire);
    let saved = meta.map_or(0, |m| m.saved_through_view_seq);
    let gaps = meta.map_or(0, |m| m.gaps.len());
    let value = RecorderStatus {
        run_epoch: epoch,
        state: if error.is_some() {
            "degraded"
        } else if saved < latest
            || shared.next.load(Ordering::Acquire) > meta.map_or(0, |m| m.saved_record_seq)
        {
            "pending"
        } else {
            "saved"
        },
        observed_view_seq: latest,
        saved_through_view_seq: saved,
        persisted_through_view_seq: meta.map_or(0, |m| m.persisted_through_view_seq),
        history_coverage: if gaps > 0 || history_partial || error.is_some() {
            "partial"
        } else {
            "complete_for_observed_scope"
        },
        gap_count: gaps,
        error,
    };
    if *shared.status.borrow() != value {
        shared.status.send_replace(value);
    }
}
fn record(
    hub: Weak<LiveHub>,
    epoch: Uuid,
    receiver: mpsc::Receiver<Packet>,
    shared: Arc<Shared>,
    options: RecorderOptions,
    mut checkpoint: Checkpoint,
) {
    let mut writer: Option<Writer> = None;
    let mut committed: Option<Meta> = None;
    let mut fault: Option<&'static str> = None;
    let mut sync_at = Instant::now();
    let mut retry_at = Instant::now();
    let started_at = chrono::Utc::now().to_rfc3339();
    loop {
        #[cfg(test)]
        {
            let pause = options.faults.pause_ms.load(Ordering::Acquire);
            if pause > 0 {
                options.faults.pause_active.store(true, Ordering::Release);
                std::thread::sleep(Duration::from_millis(pause));
                options.faults.pause_active.store(false, Ordering::Release);
            }
        }
        let stopping = shared.stop.load(Ordering::Acquire);
        if shared.lost.swap(false, Ordering::AcqRel) {
            fault = Some("queue_full");
        }
        if (writer.is_none() || fault.is_some()) && Instant::now() >= retry_at {
            let recovered = (|| -> std::io::Result<()> {
                #[cfg(test)]
                if options.faults.write_error.load(Ordering::Acquire)
                    || options.faults.sync_error.load(Ordering::Acquire)
                {
                    return Err(std::io::Error::from_raw_os_error(libc::ENOSPC));
                }
                if fault.is_some() {
                    checkpoint = hub
                        .upgrade()
                        .ok_or(std::io::ErrorKind::BrokenPipe)?
                        .recording_checkpoint();
                }
                if let Some(w) = writer.as_mut() {
                    if let Some(meta) = &committed {
                        w.meta = meta.clone();
                    }
                    w.rebase(checkpoint.clone(), fault.unwrap_or("recording_gap"))?;
                } else {
                    let root = if let Some(parent) = &options.private_parent {
                        if options.root.parent() != Some(parent.as_path()) {
                            return Err(std::io::ErrorKind::InvalidInput.into());
                        }
                        let name = options
                            .root
                            .file_name()
                            .and_then(|n| n.to_str())
                            .ok_or(std::io::ErrorKind::InvalidInput)?;
                        Directory::root(parent)?.dir(name, true)?
                    } else {
                        Directory::root(&options.root)?
                    };
                    let mut meta = Meta {
                        format_version: journal::FORMAT,
                        run_epoch: epoch,
                        workspace_id: options.workspace_id.clone(),
                        project_name: options.project_name.clone(),
                        started_at: started_at.clone(),
                        ended: false,
                        persisted_through_view_seq: 0,
                        saved_through_view_seq: 0,
                        saved_record_seq: 0,
                        segments: Vec::new(),
                        gaps: Vec::new(),
                    };
                    if checkpoint.sequence() > 0 || checkpoint.record_seq > 0 || fault.is_some() {
                        meta.gaps.push(journal::Gap {
                            after_view_seq: 0,
                            through_view_seq: checkpoint.sequence(),
                            after_record_seq: 0,
                            through_record_seq: checkpoint.record_seq,
                            reason: fault.unwrap_or("late_recording_start").into(),
                        });
                    }
                    writer = Some(Writer::create(&root, meta, checkpoint.clone())?);
                }
                committed = writer.as_ref().map(|w| w.meta.clone());
                Ok(())
            })();
            if recovered.is_err() {
                fault = Some("storage_failed");
                retry_at = Instant::now() + Duration::from_secs(1);
            } else {
                fault = None;
                sync_at = Instant::now();
            }
        }
        let mut dirty_bytes = 0;
        // A bounded batch avoids starving health/recovery/shutdown while model
        // chunks continue to arrive. recv_timeout never holds the LiveHub lock.
        for index in 0..256 {
            let packet = if index == 0 && !stopping {
                receiver.recv_timeout(Duration::from_millis(100)).ok()
            } else {
                receiver.try_recv().ok()
            };
            let Some(packet) = packet else {
                break;
            };
            if fault.is_some() || writer.is_none() {
                continue;
            }
            let w = writer.as_mut().unwrap();
            if packet.record_seq <= w.checkpoint.record_seq {
                continue;
            }
            let (kind, data) = match packet.content {
                Content::View(value, checkpoint) => {
                    dirty_bytes += value.json.len();
                    if let Some(checkpoint) = checkpoint {
                        (
                            "view_reset",
                            json!({"event":serde_json::from_str::<serde_json::Value>(&value.json).expect("closed view DTO"),"checkpoint":checkpoint}),
                        )
                    } else {
                        (
                            "view",
                            serde_json::from_str(&value.json).expect("closed view DTO"),
                        )
                    }
                }
                Content::Document {
                    request_id,
                    capture_seq,
                    document,
                } => {
                    dirty_bytes += document.bytes();
                    (
                        "document",
                        json!(Document {
                            request_id,
                            capture_seq,
                            document: json!(document)
                        }),
                    )
                }
            };
            #[cfg(test)]
            let result = if options.faults.write_error.load(Ordering::Acquire) {
                Err(std::io::Error::from_raw_os_error(libc::ENOSPC))
            } else {
                w.append(packet.record_seq, packet.view_seq, kind, data)
            };
            #[cfg(not(test))]
            let result = w.append(packet.record_seq, packet.view_seq, kind, data);
            if result.is_err() {
                fault = Some("storage_failed");
                retry_at = Instant::now() + Duration::from_secs(1);
            }
            if dirty_bytes >= 256 * 1024 {
                break;
            }
        }
        if fault.is_none()
            && let Some(w) = &mut writer
        {
            let drained = w.checkpoint.record_seq >= shared.next.load(Ordering::Acquire);
            if (sync_at.elapsed() >= options.sync_interval
                && w.checkpoint.record_seq > w.meta.saved_record_seq)
                || stopping
            {
                #[cfg(test)]
                let result = if options.faults.sync_error.load(Ordering::Acquire) {
                    Err(std::io::Error::from_raw_os_error(libc::ENOSPC))
                } else {
                    w.commit_when(|| {
                        stopping && drained && shared.clean_exit.load(Ordering::Acquire)
                    })
                };
                #[cfg(not(test))]
                let result = w.commit_when(|| {
                    stopping && drained && shared.clean_exit.load(Ordering::Acquire)
                });
                if result.is_err() {
                    fault = Some("sync_failed");
                    retry_at = Instant::now() + Duration::from_secs(1);
                } else {
                    committed = Some(w.meta.clone());
                }
                sync_at = Instant::now();
            }
            if stopping && !drained {
                continue;
            }
        }
        status(
            &shared,
            committed.as_ref(),
            epoch,
            fault,
            writer
                .as_ref()
                .is_some_and(|w| w.checkpoint.details_partial),
        );
        if stopping {
            break;
        }
    }
}

#[cfg(test)]
mod tests;
