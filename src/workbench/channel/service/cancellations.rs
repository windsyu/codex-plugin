//! One task's volatile queued-cancel authority. Never a native interrupt.
use super::*;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};

const OUTBOX_LIMIT: usize = QUEUE_LIMIT + 1;
const CANCEL_ACK: &str = "Queued task canceled before native submission.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Open = 0,
    SubmitClaimed,
    CancelReserved,
    Canceled,
    CancelFailed,
    Unknown,
    Finished,
}

// No Debug/Serialize. All phase transitions occur under Core's authorization lock.
pub(super) struct Dispatch {
    phase: AtomicU8,
    offered: AtomicBool,
}
impl Dispatch {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            phase: AtomicU8::new(Phase::Open as u8),
            offered: AtomicBool::new(false),
        })
    }
    fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Acquire) {
            0 => Phase::Open,
            1 => Phase::SubmitClaimed,
            2 => Phase::CancelReserved,
            3 => Phase::Canceled,
            4 => Phase::CancelFailed,
            5 => Phase::Unknown,
            _ => Phase::Finished,
        }
    }
    fn set(&self, phase: Phase) {
        self.phase.store(phase as u8, Ordering::Release);
    }
    pub(super) fn open(&self) -> bool {
        self.phase() == Phase::Open
    }
    pub(super) fn actor_updates(&self) -> bool {
        !matches!(
            self.phase(),
            Phase::CancelReserved | Phase::Canceled | Phase::CancelFailed
        )
    }
}

// References, exact target and native handles remain bounded and volatile.
pub(super) struct Offer {
    task: Uuid,
    active: ActiveBinding,
    source: session::Handle,
    client: Arc<Client>,
    dispatch: Arc<Dispatch>,
    reference: String,
    message: AtomicI64,
    received: Instant,
}
pub(super) enum Outbound {
    Notice(Arc<Offer>),
    Ack(Arc<Offer>),
}
impl Outbound {
    fn task(&self) -> Uuid {
        match self {
            Self::Notice(offer) | Self::Ack(offer) => offer.task,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ReceiptEnd {
    Close,
    KeepUncertain,
    OwnedByCancellation,
}

fn valid(inner: &Inner, offer: &Offer) -> bool {
    !inner.stopped
        && inner.client.is_some()
        && offer.received.elapsed() < TASK_LIFETIME
        && inner
            .binding
            .revalidate(offer.active.generation, offer.active.target)
            .is_ok()
        && offer.source.qualified(offer.active.target.thread_id) == Ok(offer.active.target)
}

fn remove(inner: &mut Inner, task: Uuid) {
    inner.cancel_offers.remove(&task);
    inner.cancel_outbox.retain(|entry| entry.task() != task);
}

pub(super) fn request_locked(inner: &mut Inner, job: &Job) {
    if !job.dispatch.open()
        || job.dispatch.offered.load(Ordering::Acquire)
        || inner.cancel_outbox.len() >= OUTBOX_LIMIT
        || inner.cancel_offers.len() >= OUTBOX_LIMIT
    {
        return;
    }
    let offer = Arc::new(Offer {
        task: job.id,
        active: job.active,
        source: job.session.clone(),
        client: job.client.clone(),
        dispatch: job.dispatch.clone(),
        reference: Uuid::new_v4().simple().to_string(),
        message: AtomicI64::new(0),
        received: job.received,
    });
    if valid(inner, &offer) {
        job.dispatch.offered.store(true, Ordering::Release);
        inner.cancel_offers.insert(job.id, offer.clone());
        inner.cancel_outbox.push_back(Outbound::Notice(offer));
    }
}
pub(super) fn request(core: &Core, job: &Job) {
    request_locked(&mut core.inner.lock().unwrap(), job);
    core.cancel_changed.notify_waiters();
}

/// Claim before the first submission-intent await. A canceled arm sends no RPC.
pub(super) fn claim(core: &Core, job: &Job) -> bool {
    let mut inner = core.inner.lock().unwrap();
    if !job.dispatch.open()
        || inner.stopped
        || inner.client.is_none()
        || inner
            .binding
            .revalidate(job.active.generation, job.active.target)
            .is_err()
    {
        return false;
    }
    job.dispatch.set(Phase::SubmitClaimed);
    remove(&mut inner, job.id);
    true
}
pub(super) fn uncertain(core: &Core, job: &Job) {
    let mut inner = core.inner.lock().unwrap();
    job.dispatch.set(Phase::Unknown);
    remove(&mut inner, job.id);
}
pub(super) fn finish(core: &Core, job: &Job) -> ReceiptEnd {
    let mut inner = core.inner.lock().unwrap();
    match job.dispatch.phase() {
        Phase::Open | Phase::SubmitClaimed => {
            job.dispatch.set(Phase::Finished);
            remove(&mut inner, job.id);
            ReceiptEnd::Close
        }
        Phase::Unknown => ReceiptEnd::KeepUncertain,
        _ => ReceiptEnd::OwnedByCancellation,
    }
}

fn command(text: &str) -> Option<&str> {
    let reference = text.strip_prefix("/cancel ")?;
    (reference.len() == 32
        && reference
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(reference)
}
fn direct(update: &serde_json::Value) -> bool {
    update["message"].as_object().is_some_and(|message| {
        [
            "forward_origin",
            "is_automatic_forward",
            "edit_date",
            "via_bot",
            "reply_to_message",
            "external_reply",
            "quote",
        ]
        .iter()
        .all(|field| !message.contains_key(*field))
    })
}

pub(super) fn pending(core: &Core, bot: i64, update: &serde_json::Value) -> bool {
    let Some(update_id) = update["update_id"].as_i64() else {
        return false;
    };
    pending_locked(
        &core.inner.lock().unwrap(),
        bot,
        update_id,
        private_message_key(bot, update),
    )
}
fn pending_locked(inner: &Inner, bot: i64, update_id: i64, key: Option<DeliveryKey>) -> bool {
    inner
        .cancel_pending
        .values()
        .any(|(pending_key, pending_update)| {
            key == Some(*pending_key) || (pending_key.bot_id == bot && *pending_update == update_id)
        })
}

fn candidate(
    inner: &mut Inner,
    bot: i64,
    generation: Uuid,
    update: &serde_json::Value,
) -> Option<(Arc<Offer>, DeliveryKey)> {
    let current = inner.binding.snapshot(Instant::now());
    if current.generation != generation || current.bot_id != Some(bot) || !direct(update) {
        return None;
    }
    let Inbound::Text {
        message_id, text, ..
    } = inner.binding.authorize(bot, generation, update).ok()?
    else {
        return None;
    };
    let reference = command(&text)?;
    let offer = inner
        .cancel_offers
        .values()
        .find(|offer| {
            offer.reference == reference
                && offer.message.load(Ordering::Acquire) > 0
                && offer.dispatch.open()
                && valid(inner, offer)
        })?
        .clone();
    let key = DeliveryKey {
        bot_id: bot,
        chat_id: offer.active.chat_id,
        message_id,
    };
    Some((offer, key))
}

pub(super) async fn cancel(
    core: &Arc<Core>,
    bot: i64,
    generation: Uuid,
    update: &serde_json::Value,
    writer: &receipts::Handle,
) -> Result<(), ()> {
    let update_id = update["update_id"].as_i64().ok_or(())?;
    let proposed = {
        let mut inner = core.inner.lock().unwrap();
        if pending_locked(&inner, bot, update_id, private_message_key(bot, update)) {
            return Ok(());
        }
        candidate(&mut inner, bot, generation, update)
    };
    let Some((offer, key)) = proposed else {
        return writer
            .reject(bot, update_id, private_message_key(bot, update))
            .await
            .map_err(|error| core.failed(error.code()));
    };
    // A known input cannot consume another task's volatile authority. This
    // read grants no authority; recheck the exact offer after the await.
    match writer.cancel_input_seen(key, update_id).await {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(error) => {
            core.failed(error.code());
            return Err(());
        }
    }
    let claimed = {
        let mut inner = core.inner.lock().unwrap();
        if pending_locked(&inner, bot, update_id, Some(key)) {
            // Its first owner will decide the durable result; do not append
            // a competing rejection or learn a second identity.
            return Ok(());
        }
        if inner.cancel_pending.len() < OUTBOX_LIMIT
            && candidate(&mut inner, bot, generation, update).is_some_and(
                |(current, current_key)| Arc::ptr_eq(&current, &offer) && current_key == key,
            )
        {
            // The winner owns the durable transition, including uncertain
            // failures. No actor may restore Open or independently Close.
            offer.dispatch.set(Phase::CancelReserved);
            inner.cancel_pending.insert(offer.task, (key, update_id));
            remove(&mut inner, offer.task);
            inner.queue.retain(|job| job.id != offer.task);
            true
        } else {
            false
        }
    };
    if !claimed {
        return writer
            .reject(bot, update_id, private_message_key(bot, update))
            .await
            .map_err(|error| core.failed(error.code()));
    }
    core.changed.notify_waiters();
    let result = writer.cancel_queued(key, update_id, offer.task).await;
    let mut inner = core.inner.lock().unwrap();
    inner.cancel_pending.remove(&offer.task);
    match result {
        Ok(Admission::Fresh) => {
            offer.dispatch.set(Phase::Canceled);
            if let Some(task) = inner
                .last_task
                .as_mut()
                .filter(|task| task.id == offer.task)
            {
                task.phase = "canceled";
                task.native_confirmed = false;
                task.native_completed = false;
                task.delivery = "notSubmitted";
                task.reason = Some("channel_queue_canceled");
            }
            if valid(&inner, &offer) && inner.cancel_outbox.len() < OUTBOX_LIMIT {
                inner.cancel_outbox.push_back(Outbound::Ack(offer));
            }
            drop(inner);
            core.cancel_changed.notify_waiters();
            Ok(())
        }
        Ok(Admission::Duplicate) => {
            offer.dispatch.set(Phase::CancelFailed);
            // A duplicate cannot authorize canceling another task; this task
            // remains unsubmitted and its Received receipt is not relabeled.
            Ok(())
        }
        Err(error) => {
            offer.dispatch.set(Phase::CancelFailed);
            drop(inner);
            core.failed(error.code());
            Err(())
        }
    }
}

pub(super) async fn deliver(core: &Core, outbound: Outbound) {
    let (offer, notice) = match outbound {
        Outbound::Notice(offer) => (offer, true),
        Outbound::Ack(offer) => (offer, false),
    };
    {
        let mut inner = core.inner.lock().unwrap();
        if !valid(&inner, &offer)
            || (notice && !offer.dispatch.open())
            || (!notice && offer.dispatch.phase() != Phase::Canceled)
        {
            if notice
                && offer.dispatch.open()
                && inner
                    .cancel_offers
                    .get(&offer.task)
                    .is_some_and(|current| Arc::ptr_eq(current, &offer))
            {
                // No HTTP attempt occurred. A busy actor can offer again after
                // transient capture backlog; Unknown sends never take this path.
                inner.cancel_offers.remove(&offer.task);
                offer.dispatch.offered.store(false, Ordering::Release);
            }
            return;
        }
    }
    let text = if notice {
        format!(
            "Task is waiting. Cancel before submission with:\n/cancel {}\nAfter submission starts, cancellation is unavailable.",
            offer.reference
        )
    } else {
        CANCEL_ACK.to_owned()
    };
    let result = offer
        .client
        .send_text(offer.active.chat_id, &text, &[])
        .await;
    let mut inner = core.inner.lock().unwrap();
    if notice {
        if let SendOutcome::Confirmed {
            chat_id,
            message_id,
        } = result
            && chat_id == offer.active.chat_id
            && valid(&inner, &offer)
            && offer.dispatch.open()
            && inner
                .cancel_offers
                .get(&offer.task)
                .is_some_and(|current| Arc::ptr_eq(current, &offer))
        {
            offer.message.store(message_id, Ordering::Release);
        } else {
            inner.cancel_offers.remove(&offer.task);
        }
    } else if let Some(task) = inner
        .last_task
        .as_mut()
        .filter(|task| task.id == offer.task)
    {
        task.delivery = match result {
            SendOutcome::Confirmed { .. } => "confirmed",
            SendOutcome::Rejected { .. } => "rejected",
            SendOutcome::Unknown => "unknown",
        };
    }
}

pub(super) async fn send_loop(core: Arc<Core>) {
    loop {
        let notified = core.cancel_changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let outbound = {
            let mut inner = core.inner.lock().unwrap();
            if inner.stopped {
                return;
            }
            inner.cancel_outbox.pop_front()
        };
        if let Some(outbound) = outbound {
            deliver(&core, outbound).await;
        } else {
            notified.await;
        }
    }
}
