//! One locally confirmed private chat, independent of native forwarding.
//! Bodies stay in a bounded volatile queue; receipts never replay native work.
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::{sync::Notify, task::JoinHandle};
use uuid::Uuid;

use super::{
    binding::{ActiveBinding, Binding, Candidate, Phase},
    receipts::{self, Counts, Worker},
    session,
    store::{Admission, DeliveryKey},
    telegram::{
        Inbound,
        client::{BotIdentity, Client, SendOutcome},
    },
};
use crate::workbench::launch::Submission;

const QUEUE_LIMIT: usize = 16;
const TASK_LIFETIME: Duration = Duration::from_secs(30 * 60);
const OFFSET_LIFETIME: Duration = Duration::from_secs(6 * 24 * 60 * 60);
mod cancellations;
mod interactions;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskView {
    pub id: Uuid,
    pub phase: &'static str,
    pub native_confirmed: bool,
    pub native_completed: bool,
    pub delivery: &'static str,
    pub reason: Option<&'static str>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub binding: super::binding::Snapshot,
    pub configured: bool,
    pub configuring: bool,
    pub queued: usize,
    pub recovered: Counts,
    pub last_task: Option<TaskView>,
    pub failure: Option<&'static str>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Pairing {
    pub generation: Uuid,
    pub command: String,
    pub expires_in_seconds: u64,
}
// No Debug/Serialize on jobs or clients: neither credentials nor bodies leave here.
struct Job {
    id: Uuid,
    active: ActiveBinding,
    session: session::Handle,
    client: Arc<Client>,
    text: String,
    received: Instant,
    dispatch: Arc<cancellations::Dispatch>,
}
struct Inner {
    binding: Binding,
    client: Option<Arc<Client>>,
    client_generation: Uuid,
    configuring: bool,
    sessions: BTreeMap<Uuid, session::Handle>,
    queue: VecDeque<Job>,
    executing: Option<Uuid>,
    cancel_offers: BTreeMap<Uuid, Arc<cancellations::Offer>>,
    cancel_outbox: VecDeque<cancellations::Outbound>,
    cancel_pending: BTreeMap<Uuid, (DeliveryKey, i64)>,
    interactions: BTreeMap<Uuid, interactions::Pending>,
    active_interaction: Option<(Uuid, ActiveBinding, session::InteractionHandle)>,
    last_task: Option<TaskView>,
    recovered: Counts,
    failure: Option<&'static str>,
    stopped: bool,
}
impl Inner {
    fn clear_queue(&mut self) {
        self.queue.clear();
        self.cancel_offers.clear();
        self.cancel_outbox.clear();
        // In-flight dedup reservations survive revocation; they grant no
        // authority and remain bounded until their durable result is known.
    }
    fn revoke(&mut self) {
        self.binding.revoke();
        self.clear_queue();
        self.interactions.clear();
        self.active_interaction = None;
    }
}
struct Core {
    inner: Mutex<Inner>,
    worker: Mutex<Option<Worker>>,
    root: PathBuf,
    changed: Notify,
    cancel_changed: Notify,
    configuring: tokio::sync::Mutex<()>,
    #[cfg(all(test, target_os = "macos"))]
    test_api: Mutex<Option<std::net::SocketAddr>>,
}
pub(crate) struct Service {
    core: Arc<Core>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}
impl Service {
    /// Resolving this path neither reads a token nor creates storage. Only a
    /// local owner action starts the receipt writer and outbound Bot API calls.
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            core: Arc::new(Core {
                inner: Mutex::new(Inner {
                    binding: Binding::new(),
                    client: None,
                    client_generation: Uuid::new_v4(),
                    configuring: false,
                    sessions: BTreeMap::new(),
                    queue: VecDeque::new(),
                    executing: None,
                    cancel_offers: BTreeMap::new(),
                    cancel_outbox: VecDeque::new(),
                    cancel_pending: BTreeMap::new(),
                    interactions: BTreeMap::new(),
                    active_interaction: None,
                    last_task: None,
                    recovered: Counts {
                        received: 0,
                        submitting: 0,
                        closed: 0,
                    },
                    failure: None,
                    stopped: false,
                }),
                worker: Mutex::new(None),
                root,
                changed: Notify::new(),
                cancel_changed: Notify::new(),
                configuring: tokio::sync::Mutex::new(()),
                #[cfg(all(test, target_os = "macos"))]
                test_api: Mutex::new(None),
            }),
            tasks: Mutex::new(Vec::new()),
        }
    }
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn api_for_test(&self, address: std::net::SocketAddr) {
        assert!(address.ip().is_loopback());
        assert!(self.core.inner.lock().unwrap().client.is_none());
        *self.core.test_api.lock().unwrap() = Some(address);
    }
    pub(crate) fn start(&self) {
        let mut tasks = self.tasks.lock().unwrap();
        if tasks.is_empty() {
            tasks.push(tokio::spawn(poll(self.core.clone())));
            tasks.push(tokio::spawn(execute(self.core.clone())));
            tasks.push(tokio::spawn(cancellations::send_loop(self.core.clone())));
        }
    }
    pub(crate) fn register(&self, session: session::Handle) {
        let run = session.snapshot().run_id;
        self.core
            .inner
            .lock()
            .unwrap()
            .sessions
            .insert(run, session);
        self.core.changed.notify_waiters();
    }
    pub(crate) fn unregister(&self, run: Uuid) {
        let mut inner = self.core.inner.lock().unwrap();
        inner.sessions.remove(&run);
        let active = inner.binding.snapshot(Instant::now()).active;
        if let Some(active) = active.filter(|active| active.target.run_epoch == run) {
            inner
                .binding
                .invalidate(active.generation, Some(active.target));
            inner.clear_queue();
            inner.failure = Some("channel_target_stopped");
        }
        drop(inner);
        self.core.changed.notify_waiters();
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        let mut inner = self.core.inner.lock().unwrap();
        if let Some(active) = inner.binding.snapshot(Instant::now()).active {
            let current = inner
                .sessions
                .get(&active.target.run_epoch)
                .map(|source| source.qualified(active.target.thread_id));
            let invalid = match current {
                Some(Ok(target)) => target != active.target,
                Some(Err(session::Failure::Backlog)) => false,
                _ => true,
            };
            if invalid {
                inner
                    .binding
                    .invalidate(active.generation, Some(active.target));
                inner.clear_queue();
                inner.failure = Some("channel_target_unavailable");
                self.core.changed.notify_waiters();
            }
        }
        Snapshot {
            binding: inner.binding.snapshot(Instant::now()),
            configured: inner.client.is_some(),
            configuring: inner.configuring,
            queued: inner.queue.len(),
            recovered: inner.recovered,
            last_task: inner.last_task,
            failure: inner.failure,
        }
    }
    /// None loads a previously saved credential after an explicit owner action.
    /// A new token is inspected before replacing the private credential file.
    pub(crate) async fn configure(&self, token: Option<String>) -> Result<Snapshot, &'static str> {
        let _guard = self
            .core
            .configuring
            .try_lock()
            .map_err(|_| "channel_configuration_busy")?;
        let operation = {
            let mut inner = self.core.inner.lock().unwrap();
            if inner.stopped {
                return Err("channel_stopped");
            }
            inner.revoke();
            inner.client = None;
            inner.client_generation = Uuid::new_v4();
            inner.configuring = true;
            inner.failure = None;
            inner.client_generation
        };
        self.core.changed.notify_waiters();
        let result = self.configure_inner(token, operation).await;
        let mut inner = self.core.inner.lock().unwrap();
        if inner.client_generation == operation {
            inner.configuring = false;
            if let Err(code) = result {
                inner.failure = Some(code);
            }
        }
        drop(inner);
        result?;
        Ok(self.snapshot())
    }
    async fn configure_inner(
        &self,
        supplied: Option<String>,
        operation: Uuid,
    ) -> Result<(), &'static str> {
        let writer = self.core.writer()?;
        let loaded = writer.load().await.map_err(|error| error.code())?;
        let save = supplied.is_some();
        let token = supplied.or(loaded.token).ok_or("channel_token_missing")?;
        let client = Client::new(token.clone()).map_err(bot_failure)?;
        // Test binaries can exercise the owner configure path against a
        // synthetic loopback Bot API. Production has no endpoint override.
        #[cfg(all(test, target_os = "macos"))]
        let client = if let Some(address) = *self.core.test_api.lock().unwrap() {
            Client::local(address, Duration::from_secs(30)).map_err(bot_failure)?
        } else {
            client
        };
        let client = Arc::new(client);
        let bot = client.inspect().await.map_err(bot_failure)?;
        self.core.configuration_current(operation)?;
        if save {
            // Revoking input authorization does not roll back a credential
            // write already started. Forgetting the credential is a separate,
            // serialized owner action.
            writer
                .save_token(token)
                .await
                .map_err(|error| error.code())?;
        }
        let mut inner = self.core.inner.lock().unwrap();
        if inner.stopped || inner.client_generation != operation {
            return Err("channel_configuration_changed");
        }
        inner
            .binding
            .replace_bot(bot)
            .map_err(|_| "channel_bot_invalid")?;
        inner.client = Some(client);
        inner.recovered = loaded.recovered;
        inner.failure = None;
        drop(inner);
        self.core.changed.notify_waiters();
        Ok(())
    }
    pub(crate) fn begin_pair(&self) -> Result<Pairing, &'static str> {
        let mut inner = self.core.inner.lock().unwrap();
        let bot_id = inner
            .binding
            .snapshot(Instant::now())
            .bot_id
            .filter(|_| inner.client.is_some() && !inner.configuring)
            .ok_or("channel_bot_unconfigured")?;
        inner.clear_queue();
        let code = inner
            .binding
            .begin_pair(BotIdentity { id: bot_id }, Instant::now())
            .map_err(|_| "channel_binding_invalid")?;
        inner.failure = None;
        drop(inner);
        self.core.changed.notify_waiters();
        Ok(Pairing {
            generation: code.generation,
            command: format!("/bind {}", code.nonce),
            expires_in_seconds: code.expires_in.as_secs(),
        })
    }
    pub(crate) fn confirm(
        &self,
        candidate: Candidate,
        run: Uuid,
        thread: Uuid,
        revision: Uuid,
        interactive_consent: bool,
    ) -> Result<Snapshot, &'static str> {
        let mut inner = self.core.inner.lock().unwrap();
        if inner.client.is_none() || inner.configuring {
            return Err("channel_bot_unconfigured");
        }
        let source = inner
            .sessions
            .get(&run)
            .ok_or("channel_target_unavailable")?;
        let target = source
            .qualified_at(thread, revision)
            .map_err(|error| error.code())?;
        inner
            .binding
            .confirm(candidate, target, interactive_consent, Instant::now())
            .map_err(|_| "channel_candidate_changed")?;
        inner.clear_queue();
        inner.failure = None;
        drop(inner);
        self.core.changed.notify_waiters();
        Ok(self.snapshot())
    }
    pub(crate) fn revoke(&self) -> Snapshot {
        let mut inner = self.core.inner.lock().unwrap();
        inner.revoke();
        // A revocation also invalidates a configuration currently awaiting HTTP.
        if inner.configuring {
            inner.client_generation = Uuid::new_v4();
            inner.configuring = false;
        }
        inner.failure = None;
        drop(inner);
        self.core.changed.notify_waiters();
        self.snapshot()
    }
    pub(crate) async fn forget(&self) -> Result<Snapshot, &'static str> {
        let _guard = self
            .core
            .configuring
            .try_lock()
            .map_err(|_| "channel_configuration_busy")?;
        {
            let mut inner = self.core.inner.lock().unwrap();
            inner.revoke();
            inner.client = None;
            inner.client_generation = Uuid::new_v4();
            inner.configuring = false;
        }
        self.core.changed.notify_waiters();
        self.core
            .writer()?
            .remove_token()
            .await
            .map_err(|error| error.code())?;
        Ok(self.snapshot())
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let mut inner = self.core.inner.lock().unwrap();
        inner.stopped = true;
        inner.revoke();
        inner.sessions.clear();
        inner.client = None;
        drop(inner);
        self.core.changed.notify_waiters();
        for task in self.tasks.get_mut().unwrap().drain(..) {
            task.abort();
        }
        self.core.worker.lock().unwrap().take();
    }
}
impl Core {
    fn writer(&self) -> Result<receipts::Handle, &'static str> {
        let mut worker = self.worker.lock().unwrap();
        if worker.is_none() {
            *worker = Some(Worker::start(self.root.clone()).map_err(|error| error.code())?);
        }
        Ok(worker.as_ref().unwrap().handle())
    }
    fn configuration_current(&self, operation: Uuid) -> Result<(), &'static str> {
        let inner = self.inner.lock().unwrap();
        if inner.stopped || inner.client_generation != operation {
            Err("channel_configuration_changed")
        } else {
            Ok(())
        }
    }
    fn valid(&self, active: ActiveBinding) -> bool {
        let inner = self.inner.lock().unwrap();
        !inner.stopped
            && inner.client.is_some()
            && inner
                .binding
                .revalidate(active.generation, active.target)
                .is_ok()
    }
    /// An uncertain old task must never revoke a subsequently confirmed binding.
    fn revoke_job(&self, active: ActiveBinding) {
        let mut inner = self.inner.lock().unwrap();
        if inner
            .binding
            .revalidate(active.generation, active.target)
            .is_ok()
        {
            inner.revoke();
            drop(inner);
            self.changed.notify_waiters();
        }
    }
    fn update(
        &self,
        job: &Job,
        phase: &'static str,
        confirmed: bool,
        completed: bool,
        delivery: &'static str,
        reason: Option<&'static str>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        if !job.dispatch.actor_updates() {
            return;
        }
        inner.last_task = Some(TaskView {
            id: job.id,
            phase,
            native_confirmed: confirmed,
            native_completed: completed,
            delivery,
            reason,
        });
    }
    fn failed(&self, code: &'static str) {
        let mut inner = self.inner.lock().unwrap();
        inner.revoke();
        inner.client = None;
        inner.client_generation = Uuid::new_v4();
        inner.configuring = false;
        inner.failure = Some(code);
        drop(inner);
        self.changed.notify_waiters();
    }
    fn failed_window(&self, version: Uuid, generation: Uuid, code: &'static str) {
        let mut inner = self.inner.lock().unwrap();
        if inner.client_generation == version
            && inner.binding.snapshot(Instant::now()).generation == generation
        {
            inner.revoke();
            inner.client = None;
            inner.failure = Some(code);
        }
        drop(inner);
        self.changed.notify_waiters();
    }
}
fn bot_failure(error: super::telegram::client::Failure) -> &'static str {
    use super::telegram::client::Failure::*;
    match error {
        InvalidToken => "channel_token_invalid",
        ExistingWebhook => "channel_webhook_exists",
        ApiRejected { .. } => "channel_bot_rejected",
        Connect | Timeout | Transport => "channel_bot_unreachable",
        _ => "channel_bot_response_invalid",
    }
}
async fn poll(core: Arc<Core>) {
    let mut client_generation = Uuid::nil();
    let mut offset_bot = None;
    let mut offset = None;
    let mut last_update = Instant::now();
    loop {
        let notified = core.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let current = {
            let mut inner = core.inner.lock().unwrap();
            if inner.stopped {
                return;
            }
            let binding = inner.binding.snapshot(Instant::now());
            inner
                .client
                .clone()
                .zip(binding.bot_id)
                .map(|(client, bot)| (client, bot, inner.client_generation, binding.generation))
        };
        let Some((client, bot, version, generation)) = current else {
            notified.await;
            continue;
        };
        if client_generation != version {
            client_generation = version;
            if offset_bot != Some(bot) {
                offset_bot = Some(bot);
                offset = None;
                last_update = Instant::now();
            }
        }
        // Telegram may randomize update IDs after a week without new updates.
        // No persistent update high-water is used as the message dedup key.
        if last_update.elapsed() >= OFFSET_LIFETIME {
            offset = None;
        }
        let batch = match tokio::select! {
            biased;
            _ = &mut notified => continue,
            batch = client.updates(offset, 20) => batch,
        } {
            Ok(batch) => batch,
            Err(error) => {
                let mut inner = core.inner.lock().unwrap();
                if inner.client_generation == version
                    && inner.binding.snapshot(Instant::now()).generation == generation
                {
                    inner.revoke();
                    inner.client = None;
                    inner.failure = Some(bot_failure(error));
                }
                drop(inner);
                core.changed.notify_waiters();
                continue;
            }
        };
        if core.configuration_current(version).is_err() {
            continue;
        }
        let mut previous = offset.and_then(|value: i64| value.checked_sub(1));
        for update in batch {
            if core.configuration_current(version).is_err() {
                break;
            }
            let Some(id) = update["update_id"]
                .as_i64()
                .filter(|id| *id >= 0 && previous.is_none_or(|prior| *id > prior))
            else {
                core.failed_window(version, generation, "channel_update_order_invalid");
                break;
            };
            let Some(next) = id.checked_add(1) else {
                core.failed_window(version, generation, "channel_update_order_invalid");
                break;
            };
            if process_update(&core, &client, bot, generation, &update)
                .await
                .is_err()
            {
                break;
            }
            // The next getUpdates confirms delivery. Executable text reaches
            // this point only after receipt fsync or durable duplicate lookup.
            offset = Some(next);
            previous = Some(id);
            last_update = Instant::now();
        }
    }
}
async fn process_update(
    core: &Arc<Core>,
    client: &Arc<Client>,
    bot: i64,
    generation: Uuid,
    update: &serde_json::Value,
) -> Result<(), ()> {
    let Some(update_id) = update["update_id"].as_i64().filter(|id| *id >= 0) else {
        core.failed("channel_update_order_invalid");
        return Err(());
    };
    let key = private_message_key(bot, update);
    if cancellations::pending(core, bot, update) {
        return Ok(());
    }
    let writer = core.writer().map_err(|code| {
        core.failed(code);
    })?;
    match writer.is_rejected(bot, update_id, key).await {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(error) => {
            core.failed(error.code());
            return Err(());
        }
    }
    if cancellations::pending(core, bot, update) {
        return Ok(());
    }
    if update.get("callback_query").is_some() {
        return interactions::callback(core, client, bot, generation, update, &writer).await;
    }
    if update.get("message").is_some_and(|message| {
        ["reply_to_message", "external_reply", "quote"]
            .iter()
            .any(|key| message.get(*key).is_some())
    }) {
        // Replies are never reinterpreted as new tasks, even after the original
        // interaction has retired or the metadata fails authorization.
        return interactions::question_reply(core, client, bot, generation, update, &writer).await;
    }
    if update["message"]["text"]
        .as_str()
        .is_some_and(|text| text.starts_with("/cancel"))
    {
        return cancellations::cancel(core, bot, generation, update, &writer).await;
    }
    // Classify under the current authorization, then release the lock before
    // fsync. Pairing candidates are published only after their input is durably
    // non-executable, so a later binding cannot reinterpret the nonce message.
    let mut pairing = false;
    let mut question_text = false;
    let executable = {
        let mut inner = core.inner.lock().unwrap();
        let current = inner.binding.snapshot(Instant::now());
        // An update received under an older authorization window is rejected,
        // even if local confirmation happened while long polling was waiting.
        if inner.stopped
            || inner.client.is_none()
            || current.generation != generation
            || current.bot_id != Some(bot)
        {
            None
        } else if current.phase == Phase::Pairing {
            pairing = true;
            None
        } else if let Some(active) = current.active
            && let Ok(Inbound::Text {
                message_id, text, ..
            }) = inner.binding.authorize(bot, generation, update)
            && !text.starts_with('/')
            && let Some(source) = inner.sessions.get(&active.target.run_epoch).cloned()
        {
            if inner
                .active_interaction
                .as_ref()
                .is_some_and(|(_, owner, handle)| {
                    owner.generation == generation && handle.awaiting_question() != Ok(false)
                })
            {
                question_text = true;
                None
            } else {
                Some((active, source, message_id, text))
            }
        } else {
            None
        }
    };
    let Some((active, source, message_id, text)) = executable else {
        if let Err(error) = writer.reject(bot, update_id, key).await {
            core.failed(error.code());
            return Err(());
        }
        if pairing {
            let mut inner = core.inner.lock().unwrap();
            let current = inner.binding.snapshot(Instant::now());
            if !inner.stopped
                && inner.client.is_some()
                && current.generation == generation
                && current.bot_id == Some(bot)
                && current.phase == Phase::Pairing
            {
                let _ = inner.binding.pair(bot, update, Instant::now());
            }
        }
        if question_text {
            let chat = core
                .inner
                .lock()
                .unwrap()
                .binding
                .snapshot(Instant::now())
                .active
                .filter(|a| a.generation == generation)
                .map(|a| a.chat_id);
            if let Some(chat) = chat {
                let _ = client.send_text(chat,
                    "This text was not queued as a new task. Use the buttons or Reply to the current question message, or answer in local Codex.", &[]).await;
            }
        }
        return Ok(());
    };
    let task = Uuid::new_v4();
    match writer
        .admit(
            DeliveryKey {
                bot_id: bot,
                chat_id: active.chat_id,
                message_id,
            },
            update_id,
            task,
        )
        .await
    {
        Ok(Admission::Duplicate) => return Ok(()),
        Ok(Admission::Fresh) => {}
        Err(error) => {
            core.failed(error.code());
            return Err(());
        }
    }
    let queued = {
        let mut inner = core.inner.lock().unwrap();
        if inner.stopped || inner.binding.revalidate(generation, active.target).is_err() {
            false
        } else if inner.queue.len() >= QUEUE_LIMIT {
            inner.failure = Some("channel_queue_full");
            false
        } else {
            let job = Job {
                id: task,
                active,
                session: source,
                client: client.clone(),
                text,
                received: Instant::now(),
                dispatch: cancellations::Dispatch::new(),
            };
            if inner.executing.is_some()
                || !inner.queue.is_empty()
                || job.session.ready(active.target) != Ok(true)
            {
                cancellations::request_locked(&mut inner, &job);
            }
            inner.queue.push_back(job);
            true
        }
    };
    if queued {
        core.changed.notify_waiters();
        core.cancel_changed.notify_waiters();
    } else if let Err(error) = writer.close(task).await {
        core.failed(error.code());
        return Err(());
    }
    Ok(())
}
fn private_message_key(bot_id: i64, update: &serde_json::Value) -> Option<DeliveryKey> {
    let message = update
        .get("message")
        .or_else(|| update.get("edited_message"))?;
    let chat = message.get("chat")?;
    if chat.get("type")?.as_str()? != "private" || bot_id <= 0 {
        return None;
    }
    Some(DeliveryKey {
        bot_id,
        chat_id: chat.get("id")?.as_i64().filter(|id| *id > 0)?,
        message_id: message.get("message_id")?.as_i64().filter(|id| *id > 0)?,
    })
}
async fn execute(core: Arc<Core>) {
    loop {
        let notified = core.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let job = {
            let mut inner = core.inner.lock().unwrap();
            if inner.stopped {
                return;
            }
            let job = inner.queue.pop_front();
            inner.executing = job.as_ref().map(|job| job.id);
            job
        };
        let Some(job) = job else {
            notified.await;
            continue;
        };
        let end = execute_job(&core, &job).await;
        // Receipt ownership follows this job, never the global last-task view.
        if end == cancellations::ReceiptEnd::Close
            && let Ok(writer) = core.writer()
            && let Err(error) = writer.close(job.id).await
        {
            core.failed(error.code());
        }
        let mut inner = core.inner.lock().unwrap();
        if inner.executing == Some(job.id) {
            inner.executing = None;
        }
    }
}
async fn execute_job(core: &Arc<Core>, job: &Job) -> cancellations::ReceiptEnd {
    run_job(core, job).await;
    cancellations::finish(core, job)
}
async fn run_job(core: &Arc<Core>, job: &Job) {
    if !job.dispatch.open() {
        return;
    }
    core.update(job, "waiting", false, false, "pending", None);
    let client_id = Uuid::new_v4();
    let mut attempt = loop {
        if !job.dispatch.open() {
            return;
        }
        if !core.valid(job.active) || job.received.elapsed() >= TASK_LIFETIME {
            core.update(
                job,
                "abandoned",
                false,
                false,
                "withheld",
                Some("channel_binding_changed"),
            );
            return;
        }
        match job.session.ready(job.active.target) {
            Ok(false) | Err(session::Failure::Backlog) => {
                cancellations::request(core, job);
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(error) => {
                core.update(
                    job,
                    "abandoned",
                    false,
                    false,
                    "withheld",
                    Some(error.code()),
                );
                return;
            }
            Ok(true) => {}
        }
        match job.session.arm(job.active.target, client_id) {
            Ok(attempt) => break attempt,
            Err(
                session::Failure::Busy
                | session::Failure::Backlog
                | session::Failure::AttemptActive,
            ) => {
                cancellations::request(core, job);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => {
                core.update(
                    job,
                    "abandoned",
                    false,
                    false,
                    "withheld",
                    Some(error.code()),
                );
                return;
            }
        }
    };
    if !cancellations::claim(core, job) {
        return;
    }
    // Register before receipt I/O and the native submit await: native questions
    // can arrive while the turn/start acknowledgment is still outstanding.
    let interactions = attempt.interactions();
    let cleanup =
        interactions::Cleanup::new(core.clone(), job.id, job.active, interactions.clone());
    let writer = match core.writer() {
        Ok(writer) => writer,
        Err(code) => {
            cancellations::uncertain(core, job);
            core.failed(code);
            return;
        }
    };
    if let Err(error) = writer.mark_submitting(job.id, client_id).await {
        cancellations::uncertain(core, job);
        core.failed(error.code());
        return;
    }
    if !core.valid(job.active) {
        core.update(
            job,
            "abandoned",
            false,
            false,
            "withheld",
            Some("channel_binding_changed"),
        );
        return;
    }
    core.update(job, "submitting", false, false, "pending", None);
    match attempt.submit(job.text.clone()).await {
        Submission::Accepted { .. } => core.update(job, "running", true, false, "pending", None),
        Submission::Unknown { .. } => {
            cancellations::uncertain(core, job);
            core.revoke_job(job.active);
            core.update(
                job,
                "uncertain",
                false,
                false,
                "withheld",
                Some("channel_submission_unknown"),
            );
            return;
        }
        Submission::Rejected { .. } | Submission::NotSent { .. } => {
            core.update(
                job,
                "rejected",
                false,
                false,
                "withheld",
                Some("channel_submission_rejected"),
            );
            return;
        }
    }
    let mut completion = Box::pin(attempt.completion());
    let result = loop {
        tokio::select! {
            result = &mut completion => break result,
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                if !core.valid(job.active) || job.received.elapsed() >= TASK_LIFETIME {
                    if interactions.awaiting_question() != Ok(false) {
                        core.revoke_job(job.active);
                    }
                    core.update(job, "abandoned", true, false, "withheld", Some("channel_binding_changed")); return;
                }
                if job.active.interactive_consent {
                    interactions::offer(core, job, &interactions).await;
                }
            }
        }
    };
    drop(completion);
    let completed = match result {
        Ok(completed) => completed,
        Err(error) => {
            if interactions.awaiting_question() != Ok(false) {
                core.revoke_job(job.active);
            }
            core.update(job, "withheld", true, false, "withheld", Some(error.code()));
            return;
        }
    };
    if completed.status != super::NativeStatus::Completed || completed.withheld.is_some() {
        if interactions.awaiting_question() != Ok(false) {
            core.revoke_job(job.active);
        }
        core.update(
            job,
            "withheld",
            true,
            true,
            "withheld",
            Some("channel_origin_withheld"),
        );
        return;
    }
    drop(cleanup);
    let Some(reply) = completed.reply else {
        core.update(job, "completed", true, true, "noReply", None);
        return;
    };
    let chunks = match split_reply(&reply) {
        Ok(chunks) => chunks,
        Err(_) => {
            core.update(
                job,
                "withheld",
                true,
                true,
                "withheld",
                Some("channel_reply_limit"),
            );
            return;
        }
    };
    for (index, chunk) in chunks.iter().enumerate() {
        if !core.valid(job.active) || attempt.revalidate().is_err() {
            core.update(
                job,
                "withheld",
                true,
                true,
                if index == 0 { "withheld" } else { "partial" },
                Some("channel_delivery_invalidated"),
            );
            return;
        }
        // There is exactly one send attempt per chunk, no automatic retry.
        match job.client.send_text(job.active.chat_id, chunk, &[]).await {
            SendOutcome::Confirmed { .. } => {}
            SendOutcome::Rejected { .. } => {
                core.update(
                    job,
                    "completed",
                    true,
                    true,
                    if index == 0 { "rejected" } else { "partial" },
                    Some("channel_send_rejected"),
                );
                return;
            }
            SendOutcome::Unknown => {
                core.update(
                    job,
                    "completed",
                    true,
                    true,
                    if index == 0 {
                        "unknown"
                    } else {
                        "partialUnknown"
                    },
                    Some("channel_send_unknown"),
                );
                return;
            }
        }
    }
    core.update(job, "completed", true, true, "confirmed", None);
}

fn split_reply(text: &str) -> Result<Vec<&str>, ()> {
    if text.is_empty()
        || text.len() > 64 * 1024
        || text
            .chars()
            .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    {
        return Err(());
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut units = 0;
    for (index, ch) in text.char_indices() {
        if units + ch.len_utf16() > 4096 {
            chunks.push(&text[start..index]);
            start = index;
            units = 0;
        }
        units += ch.len_utf16();
    }
    chunks.push(&text[start..]);
    Ok(chunks)
}

#[cfg(test)]
mod tests;
