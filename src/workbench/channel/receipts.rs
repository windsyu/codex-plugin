//! Bounded asynchronous access to the channel-only receipt store.
//!
//! All filesystem work, including opening the store, stays on one OS thread.
//! Dropping a caller never retries work. Dropping the owner stops admission and
//! discards queued work; an already-running filesystem operation may still finish.
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::JoinHandle,
    time::Duration,
};

use serde::Serialize;
use tokio::sync::oneshot;
use uuid::Uuid;

use super::store::{self, Admission, DeliveryKey, Status, Store};

const QUEUE_LIMIT: usize = 32;
const STOP_POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Counts {
    pub received: usize,
    pub submitting: usize,
    pub closed: usize,
}
// A credential may be present: never Debug or serialize this wrapper.
pub(crate) struct Loaded {
    pub token: Option<String>,
    pub recovered: Counts,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    QueueFull,
    Stopped,
    WorkerUnavailable,
    Store(store::Failure),
}
impl Failure {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::QueueFull => "channel_receipts_queue_full",
            Self::Stopped => "channel_receipts_stopped",
            Self::WorkerUnavailable => "channel_receipts_worker_unavailable",
            Self::Store(store::Failure::InvalidInput) => "channel_receipts_invalid_input",
            Self::Store(store::Failure::InvalidTransition) => "channel_receipts_invalid_transition",
            Self::Store(store::Failure::Busy) => "channel_receipts_locked",
            Self::Store(store::Failure::Unavailable) => "channel_receipts_unavailable",
            Self::Store(store::Failure::Corrupt) => "channel_receipts_corrupt",
            Self::Store(store::Failure::Capacity) => "channel_receipts_capacity",
            Self::Store(store::Failure::Poisoned) => "channel_receipts_poisoned",
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Failure {}

type Reply<T> = oneshot::Sender<Result<T, Failure>>;
// No Debug: SaveToken contains a credential. Other operations contain IDs only.
enum Operation {
    Load(Reply<Loaded>),
    IsRejected {
        bot: i64,
        update: i64,
        key: Option<DeliveryKey>,
        reply: Reply<bool>,
    },
    Reject {
        bot: i64,
        update: i64,
        key: Option<DeliveryKey>,
        reply: Reply<()>,
    },
    Admit {
        key: DeliveryKey,
        update_id: i64,
        task: Uuid,
        reply: Reply<Admission>,
    },
    Submitting {
        task: Uuid,
        client: Uuid,
        reply: Reply<()>,
    },
    Close {
        task: Uuid,
        reply: Reply<()>,
    },
    SaveToken {
        token: String,
        reply: Reply<()>,
    },
    RemoveToken(Reply<()>),
    Wake,
    #[cfg(test)]
    Pause {
        entered: oneshot::Sender<()>,
        resume: Receiver<()>,
    },
    #[cfg(test)]
    PauseAfterIntent {
        task: Uuid,
        client: Uuid,
        entered: oneshot::Sender<()>,
        resume: Receiver<()>,
        reply: Reply<()>,
    },
}
impl Operation {
    fn cancelled(&self) -> bool {
        match self {
            Self::Load(reply) => reply.is_closed(),
            Self::IsRejected { reply, .. } => reply.is_closed(),
            Self::Reject { reply, .. } => reply.is_closed(),
            Self::Admit { reply, .. } => reply.is_closed(),
            Self::Submitting { reply, .. }
            | Self::Close { reply, .. }
            | Self::SaveToken { reply, .. }
            | Self::RemoveToken(reply) => reply.is_closed(),
            Self::Wake => false,
            #[cfg(test)]
            Self::Pause { .. } => false,
            #[cfg(test)]
            Self::PauseAfterIntent { reply, .. } => reply.is_closed(),
        }
    }
}
#[derive(Clone)]
pub(crate) struct Handle {
    sender: SyncSender<Operation>,
    stopped: Arc<AtomicBool>,
}
impl Handle {
    fn send(&self, operation: Operation) -> Result<(), Failure> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(Failure::Stopped);
        }
        self.sender
            .try_send(operation)
            .map_err(|error| match error {
                TrySendError::Full(_) => Failure::QueueFull,
                TrySendError::Disconnected(_) => Failure::WorkerUnavailable,
            })
    }
    async fn call<T>(&self, build: impl FnOnce(Reply<T>) -> Operation) -> Result<T, Failure> {
        let (reply, result) = oneshot::channel();
        self.send(build(reply))?;
        result.await.map_err(|_| {
            if self.stopped.load(Ordering::Acquire) {
                Failure::Stopped
            } else {
                Failure::WorkerUnavailable
            }
        })?
    }
    pub(crate) async fn load(&self) -> Result<Loaded, Failure> {
        self.call(Operation::Load).await
    }
    pub(crate) async fn is_rejected(
        &self,
        bot: i64,
        update: i64,
        key: Option<DeliveryKey>,
    ) -> Result<bool, Failure> {
        self.call(|reply| Operation::IsRejected {
            bot,
            update,
            key,
            reply,
        })
        .await
    }
    pub(crate) async fn reject(
        &self,
        bot: i64,
        update: i64,
        key: Option<DeliveryKey>,
    ) -> Result<(), Failure> {
        self.call(|reply| Operation::Reject {
            bot,
            update,
            key,
            reply,
        })
        .await
    }
    pub(crate) async fn admit(
        &self,
        key: DeliveryKey,
        update_id: i64,
        task: Uuid,
    ) -> Result<Admission, Failure> {
        self.call(|reply| Operation::Admit {
            key,
            update_id,
            task,
            reply,
        })
        .await
    }
    pub(crate) async fn mark_submitting(&self, task: Uuid, client: Uuid) -> Result<(), Failure> {
        self.call(|reply| Operation::Submitting {
            task,
            client,
            reply,
        })
        .await
    }
    pub(crate) async fn close(&self, task: Uuid) -> Result<(), Failure> {
        self.call(|reply| Operation::Close { task, reply }).await
    }
    pub(crate) async fn save_token(&self, token: String) -> Result<(), Failure> {
        // Bound queued credential bytes as well as operation count. Store checks
        // the same format again when it actually writes the credential.
        if !super::telegram::valid_token(&token) {
            return Err(Failure::Store(store::Failure::InvalidInput));
        }
        self.call(|reply| Operation::SaveToken { token, reply })
            .await
    }
    pub(crate) async fn remove_token(&self) -> Result<(), Failure> {
        self.call(Operation::RemoveToken).await
    }
}

pub(crate) struct Worker {
    handle: Handle,
    // Never join on Drop: shutdown can happen on a web/runtime thread while the
    // filesystem is slow. The loop owns Store and releases it when its I/O returns.
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    /// Only thread creation happens synchronously. Store-open errors are returned
    /// asynchronously by load/other operations; a failed open is never retried.
    pub(crate) fn start(root: PathBuf) -> Result<Self, Failure> {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_LIMIT);
        let stopped = Arc::new(AtomicBool::new(false));
        let handle = Handle {
            sender,
            stopped: stopped.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("channel-receipts".into())
            .spawn(move || serve(root, receiver, stopped))
            .map_err(|_| Failure::WorkerUnavailable)?;
        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }
    pub(crate) fn handle(&self) -> Handle {
        self.handle.clone()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.handle.stopped.store(true, Ordering::Release);
        let _ = self.handle.sender.try_send(Operation::Wake);
        // Dropping JoinHandle detaches, never waits. Live Handle clones cannot
        // retain the writer indefinitely: stop is checked independently of EOF.
        self.thread.take();
    }
}
struct StoppedOnExit(Arc<AtomicBool>);
impl Drop for StoppedOnExit {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
fn serve(root: PathBuf, receiver: Receiver<Operation>, stopped: Arc<AtomicBool>) {
    let _done = StoppedOnExit(stopped.clone());
    if stopped.load(Ordering::Acquire) {
        return;
    }
    let mut store = Store::open(&root);
    while !stopped.load(Ordering::Acquire) {
        let operation = match receiver.recv_timeout(STOP_POLL) {
            Ok(operation) => operation,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if stopped.load(Ordering::Acquire) {
            break;
        }
        if operation.cancelled() {
            continue;
        }
        execute(operation, &mut store);
    }
    // Receiver drop releases pending oneshots and credential buffers. Never drain
    // queued operations into the filesystem after the owner has stopped.
}
fn ready(store: &mut Result<Store, store::Failure>) -> Result<&mut Store, Failure> {
    store.as_mut().map_err(|error| Failure::Store(*error))
}
fn load(store: &mut Store) -> Result<Loaded, store::Failure> {
    let mut recovered = Counts::default();
    for (_, status) in store.recover()? {
        match status {
            Status::Received => recovered.received += 1,
            Status::Submitting => recovered.submitting += 1,
            Status::Closed => recovered.closed += 1,
        }
    }
    Ok(Loaded {
        token: store.token()?,
        recovered,
    })
}
fn execute(operation: Operation, store: &mut Result<Store, store::Failure>) {
    match operation {
        Operation::IsRejected {
            bot,
            update,
            key,
            reply,
        } => {
            let _ = reply.send(
                ready(store)
                    .and_then(|store| store.is_rejected(bot, update, key).map_err(Failure::Store)),
            );
        }
        Operation::Reject {
            bot,
            update,
            key,
            reply,
        } => {
            let _ = reply.send(
                ready(store)
                    .and_then(|store| store.reject(bot, update, key).map_err(Failure::Store)),
            );
        }
        Operation::Load(reply) => {
            let _ = reply.send(ready(store).and_then(|store| load(store).map_err(Failure::Store)));
        }
        Operation::Admit {
            key,
            update_id,
            task,
            reply,
        } => {
            let _ = reply.send(
                ready(store)
                    .and_then(|store| store.admit(key, update_id, task).map_err(Failure::Store)),
            );
        }
        Operation::Submitting {
            task,
            client,
            reply,
        } => {
            let _ = reply.send(
                ready(store)
                    .and_then(|store| store.mark_submitting(task, client).map_err(Failure::Store)),
            );
        }
        Operation::Close { task, reply } => {
            let _ = reply
                .send(ready(store).and_then(|store| store.close(task).map_err(Failure::Store)));
        }
        Operation::SaveToken { token, reply } => {
            let _ = reply.send(
                ready(store).and_then(|store| store.save_token(&token).map_err(Failure::Store)),
            );
        }
        Operation::RemoveToken(reply) => {
            let _ = reply
                .send(ready(store).and_then(|store| store.remove_token().map_err(Failure::Store)));
        }
        Operation::Wake => {}
        #[cfg(test)]
        Operation::Pause { entered, resume } => {
            let _ = entered.send(());
            let _ = resume.recv();
        }
        #[cfg(test)]
        Operation::PauseAfterIntent {
            task,
            client,
            entered,
            resume,
            reply,
        } => {
            let result = ready(store)
                .and_then(|store| store.mark_submitting(task, client).map_err(Failure::Store));
            let _ = entered.send(());
            let _ = resume.recv();
            let _ = reply.send(result);
        }
    }
}

#[cfg(test)]
#[path = "receipts/tests.rs"]
mod tests;
