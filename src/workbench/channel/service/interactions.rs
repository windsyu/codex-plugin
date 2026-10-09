//! Volatile one-use references. Durable receipts contain no request or answer bodies.
use super::*;
use crate::workbench::{
    channel::{
        interaction::{Action, Request},
        telegram::client::Button,
    },
    launch::ResponseWrite,
};

const INTERACTION_LIMIT: usize = 32;

// No Debug or Serialize: commands, paths, questions and callbacks are private.
pub(super) struct Pending {
    task: Uuid,
    active: ActiveBinding,
    handle: session::InteractionHandle,
    request: Request,
    selected: Vec<(usize, usize)>,
    references: BTreeMap<String, Action>,
    message: i64,
    received: Instant,
}

pub(super) struct Cleanup {
    core: Arc<Core>,
    task: Uuid,
}
impl Cleanup {
    pub(super) fn new(
        core: Arc<Core>,
        task: Uuid,
        active: ActiveBinding,
        handle: session::InteractionHandle,
    ) -> Self {
        core.inner.lock().unwrap().active_interaction = Some((task, active, handle));
        Self { core, task }
    }
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        let mut inner = self.core.inner.lock().unwrap();
        inner.interactions.retain(|_, p| p.task != self.task);
        if inner
            .active_interaction
            .as_ref()
            .is_some_and(|(task, _, _)| *task == self.task)
        {
            inner.active_interaction = None;
        }
    }
}

fn live(core: &Core, id: Uuid, pending: &Pending) -> bool {
    pending.active.interactive_consent
        && pending.received.elapsed() < TASK_LIFETIME
        && core.valid(pending.active)
        && pending.handle.current(id).is_ok()
}

fn buttons(
    request: &Request,
    selected: &[(usize, usize)],
) -> (Vec<Button>, BTreeMap<String, Action>) {
    // Questions are answered one at a time; no partial native result is sent.
    let actions = request.actions();
    let next = actions
        .iter()
        .filter_map(|a| match a {
            Action::Choose { question, .. } if !selected.iter().any(|(q, _)| q == question) => {
                Some(*question)
            }
            _ => None,
        })
        .min();
    let mut references = BTreeMap::new();
    let mut buttons = Vec::new();
    for action in actions {
        let label = match action {
            Action::Accept => "Allow once".to_owned(),
            Action::Decline => "Deny".to_owned(),
            Action::Choose { question, option } if Some(question) == next => {
                format!("{}.{}", question + 1, option + 1)
            }
            _ => continue,
        };
        let reference = Uuid::new_v4().simple().to_string();
        references.insert(reference.clone(), action);
        buttons.push(Button { label, reference });
    }
    (buttons, references)
}

async fn send(core: &Arc<Core>, client: &Client, id: Uuid, mut pending: Pending) {
    if !live(core, id, &pending) {
        return;
    }
    let (buttons, references) = buttons(&pending.request, &pending.selected);
    if buttons.is_empty() || buttons.len() > 4 {
        return;
    }
    // Pure text, with no parse_mode, raw JSON, cwd, output, or file diffs.
    let mut text = pending.request.text();
    let progress = "\nEarlier choices recorded. Choose the next numbered answer.";
    if !pending.selected.is_empty()
        && text.encode_utf16().count() + progress.encode_utf16().count() <= 4096
    {
        text.push_str(progress);
    }
    let outcome = client
        .send_text(pending.active.chat_id, &text, &buttons)
        .await;
    let (chat_id, message_id) = match outcome {
        SendOutcome::Confirmed {
            chat_id,
            message_id,
        } => (chat_id, message_id),
        SendOutcome::Unknown | SendOutcome::Rejected { .. } => {
            let reason = if matches!(outcome, SendOutcome::Unknown) {
                "channel_interaction_send_unknown"
            } else {
                "channel_interaction_send_rejected"
            };
            let mut inner = core.inner.lock().unwrap();
            if let Some(task) = inner.last_task.as_mut().filter(|t| t.id == pending.task) {
                task.reason = Some(reason);
            }
            // Even an ambiguous send never creates actionable references or retries.
            return;
        }
    };
    if chat_id != pending.active.chat_id {
        return;
    }
    pending.message = message_id;
    pending.references = references;
    let mut inner = core.inner.lock().unwrap();
    if !inner.stopped
        && pending.active.interactive_consent
        && inner
            .binding
            .revalidate(pending.active.generation, pending.active.target)
            .is_ok()
        && pending.received.elapsed() < TASK_LIFETIME
        && pending.handle.current(id).is_ok()
        && inner.interactions.len() < INTERACTION_LIMIT
    {
        inner.interactions.insert(id, pending);
    }
}

pub(super) async fn offer(core: &Arc<Core>, job: &Job, handle: &session::InteractionHandle) {
    {
        let mut inner = core.inner.lock().unwrap();
        inner
            .interactions
            .retain(|id, p| p.handle.current(*id).is_ok());
    }
    if !core.valid(job.active) || !job.active.interactive_consent {
        return;
    }
    if let Ok(Some(prompt)) = handle.next() {
        send(
            core,
            &job.client,
            prompt.id,
            Pending {
                task: job.id,
                active: job.active,
                handle: handle.clone(),
                request: prompt.request,
                selected: Vec::new(),
                references: BTreeMap::new(),
                message: 0,
                received: job.received,
            },
        )
        .await;
    }
    if handle.take_local_notice() == Ok(true) && core.valid(job.active) {
        let _ = job.client.send_text(job.active.chat_id,
            "A native interaction needs a local response. This channel cannot safely answer it.", &[]).await;
    }
}

pub(super) async fn callback(
    core: &Arc<Core>,
    client: &Arc<Client>,
    bot: i64,
    generation: Uuid,
    update: &serde_json::Value,
    writer: &receipts::Handle,
) -> Result<(), ()> {
    let update_id = update["update_id"].as_i64().ok_or(())?;
    let claimed = {
        let mut inner = core.inner.lock().unwrap();
        let current = inner.binding.snapshot(Instant::now());
        if inner.stopped
            || inner.client.is_none()
            || current.generation != generation
            || current.bot_id != Some(bot)
        {
            None
        } else if let Ok(Inbound::Callback {
            query_id,
            message_id,
            reference,
            ..
        }) = inner.binding.authorize(bot, generation, update)
        {
            let found = inner.interactions.iter().find_map(|(id, p)| {
                (p.active.interactive_consent
                    && p.active.generation == generation
                    && p.message == message_id
                    && p.received.elapsed() < TASK_LIFETIME
                    && p.handle.current(*id).is_ok())
                .then(|| {
                    p.references
                        .get(&reference)
                        .copied()
                        .map(|action| (*id, action))
                })
                .flatten()
            });
            // Reserve every sibling reference before the first await. No second
            // click can race receipt fsync, and failures never restore authority.
            found.map(|(id, action)| {
                (
                    id,
                    action,
                    query_id,
                    inner.interactions.remove(&id).unwrap(),
                )
            })
        } else {
            None
        }
    };
    let Some((id, action, query_id, mut pending)) = claimed else {
        return writer.reject(bot, update_id, None).await.map_err(|error| {
            core.failed(error.code());
        });
    };
    match writer
        .admit_callback(bot, update_id, query_id.clone(), pending.task, id)
        .await
    {
        Ok(Admission::Duplicate) => return Ok(()),
        Ok(Admission::Fresh) => {}
        Err(error) => {
            core.failed(error.code());
            return Err(());
        }
    }
    if !live(core, id, &pending) {
        return Ok(());
    }
    match pending.request.response(&action, &pending.selected) {
        Ok(None) => {
            let Action::Choose { question, option } = action else {
                return Ok(());
            };
            pending.selected.push((question, option));
            send(core, client, id, pending).await;
        }
        Ok(Some(_)) => {
            let active = pending.active;
            let received = pending.received;
            let guard = core.clone();
            let result = pending
                .handle
                .answer(
                    id,
                    action,
                    pending.selected,
                    Box::new(move || {
                        active.interactive_consent
                            && received.elapsed() < TASK_LIFETIME
                            && guard.valid(active)
                    }),
                )
                .await;
            // The native protocol offers no application ACK or winner identity.
            // A write never becomes a claim that the approval took effect.
            let reason = match result {
                ResponseWrite::WrittenUnconfirmed => "channel_interaction_written_unconfirmed",
                ResponseWrite::NotSent { .. } => "channel_interaction_withheld",
                ResponseWrite::Unknown { .. } => "channel_interaction_write_unknown",
            };
            let mut inner = core.inner.lock().unwrap();
            if let Some(task) = inner.last_task.as_mut().filter(|t| t.id == pending.task) {
                task.reason = Some(reason);
            }
        }
        Err(_) => {}
    }
    // Spinner ACK is deliberately after durable input handling, and is never
    // interpreted as native execution or Telegram delivery confirmation.
    let _ = client.answer_callback(&query_id).await;
    Ok(())
}

#[cfg(test)]
mod tests;
