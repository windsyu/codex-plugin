//! Continuous subscribed-source qualification and a single bounded origin attempt.
//! The observer never waits for RPC, persistence, or Telegram delivery.
use super::{
    Completion, Evidence, NativeStatus, OriginProof,
    binding::QualifiedTarget,
    native::{CaptureMonitor, CaptureReceiver, CaptureState, Direction},
    subscription::Tracker,
};
use crate::workbench::launch::{ControlFailure, ControlHandle, NativeAttachment, Submission};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::{sync::Notify, task::JoinHandle};
use uuid::Uuid;
mod evidence;
const FRAME_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Unavailable,
    Unqualified,
    SourceChanged,
    Backlog,
    Busy,
    AttemptActive,
    StaleAttempt,
    InvalidEvidence,
    Submission,
    CompletionTaken,
}
impl Failure {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "channel_unavailable",
            Self::Unqualified => "channel_unqualified",
            Self::SourceChanged => "channel_source_changed",
            Self::Backlog => "channel_capture_backlog",
            Self::Busy => "channel_target_busy",
            Self::AttemptActive => "channel_attempt_active",
            Self::StaleAttempt => "channel_attempt_stale",
            Self::InvalidEvidence => "channel_evidence_invalid",
            Self::Submission => "channel_submission_failed",
            Self::CompletionTaken => "channel_completion_taken",
        }
    }
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Target {
    pub thread_id: Uuid,
    pub busy: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub run_id: Uuid,
    pub revision: Uuid,
    pub transport_epoch: Option<Uuid>,
    pub phase: &'static str,
    pub targets: Vec<Target>,
    pub failure: Option<&'static str>,
}
struct PendingAttempt {
    id: Uuid,
    target: QualifiedTarget,
    client: Uuid,
    proof: OriginProof,
    submitted: bool,
    acknowledged: bool,
    acknowledged_turn: Option<String>,
    terminal: Option<(String, NativeStatus)>,
    completion: Option<Completion>,
    taken: bool,
    failure: Option<Failure>,
}
impl PendingAttempt {
    fn fail(&mut self, failure: Failure) {
        self.failure.get_or_insert(failure);
        self.completion = None;
    }
    fn begin_submit(&mut self, eligible: bool) -> bool {
        if !eligible || self.submitted || self.failure.is_some() {
            self.fail(Failure::Submission);
            return false;
        }
        self.submitted = true;
        true
    }
    fn event(&mut self, event: Evidence<'_>) {
        if self.failure.is_some() {
            return;
        }
        if let Evidence::UserStarted { client, .. } | Evidence::UserCompleted { client, .. } = event
            && client != Some(self.client.to_string().as_str())
        {
            self.fail(Failure::InvalidEvidence);
            return;
        }
        if let Evidence::TurnCompleted { turn, status, .. } = event {
            self.terminal = Some((turn.to_owned(), status));
        }
        if let Some(completion) = self.proof.observe(event) {
            self.completion = Some(completion)
        }
        if self.proof.invalidated().is_some() {
            self.fail(Failure::InvalidEvidence)
        }
    }
    fn acknowledge(&mut self, submission: Submission) {
        match submission {
            Submission::Accepted { turn_id } => {
                self.event(Evidence::SubmissionTurn(&turn_id.to_string()));
                self.acknowledged = true;
                self.acknowledged_turn = Some(turn_id.to_string());
            }
            _ => self.fail(Failure::Submission),
        }
    }
    fn take_completion(&mut self) -> Result<Option<Completion>, Failure> {
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        if self.taken {
            return Err(Failure::CompletionTaken);
        }
        if !self.acknowledged {
            return Ok(None);
        }
        if self.completion.is_none()
            && let Some((turn, status)) = &self.terminal
            && self.acknowledged_turn.as_ref() == Some(turn)
        {
            self.completion = Some(Completion {
                status: *status,
                reply: None,
                withheld: Some(super::Withheld::IncompleteEvidence),
            });
        }
        if let Some(completion) = self.completion.take() {
            self.taken = true;
            Ok(Some(completion))
        } else {
            Ok(None)
        }
    }
}
struct State {
    revision: Uuid,
    epoch: Option<Uuid>,
    sequence: u64,
    targets: Vec<Target>,
    generations: BTreeMap<Uuid, Uuid>,
    failure: Option<&'static str>,
    attempt: Option<PendingAttempt>,
}
impl State {
    fn new() -> Self {
        Self {
            revision: Uuid::new_v4(),
            epoch: None,
            sequence: 0,
            targets: Vec::new(),
            generations: BTreeMap::new(),
            failure: None,
            attempt: None,
        }
    }
    fn fail(&mut self, code: &'static str) {
        if self.failure.is_none() {
            self.revision = Uuid::new_v4();
            self.failure = Some(code)
        }
        self.targets.clear();
        self.generations.clear();
        if let Some(attempt) = &mut self.attempt {
            attempt.fail(Failure::Unavailable)
        }
    }
    fn arm(
        &mut self,
        run_id: Uuid,
        target: QualifiedTarget,
        client: Uuid,
    ) -> Result<Uuid, Failure> {
        if !self.check_target(run_id, target)? {
            return Err(Failure::Busy);
        }
        if client.is_nil() {
            return Err(Failure::InvalidEvidence);
        }
        if self.attempt.is_some() {
            return Err(Failure::AttemptActive);
        }
        let id = Uuid::new_v4();
        self.attempt = Some(PendingAttempt {
            id,
            target,
            client,
            proof: OriginProof::new(&target.thread_id.to_string(), client)
                .ok_or(Failure::InvalidEvidence)?,
            submitted: false,
            acknowledged: false,
            acknowledged_turn: None,
            terminal: None,
            completion: None,
            taken: false,
            failure: None,
        });
        Ok(id)
    }
    fn remove_attempt(&mut self, id: Uuid) {
        if self.attempt.as_ref().is_some_and(|a| a.id == id) {
            self.attempt = None;
        }
    }
    fn check_source(&mut self, capture: CaptureState, closed: bool) -> Result<(), Failure> {
        if let Some(code) = invalid(capture, self.epoch) {
            self.fail(code)
        }
        if closed {
            self.fail("channel_control_closed")
        }
        if self.failure.is_some() {
            return Err(Failure::Unavailable);
        }
        if self.epoch.is_none() {
            return Err(Failure::Unqualified);
        }
        if self.sequence != capture.captured_sequence {
            return Err(Failure::Backlog);
        }
        Ok(())
    }
    fn check_target(&self, run_id: Uuid, target: QualifiedTarget) -> Result<bool, Failure> {
        if target.run_epoch != run_id
            || Some(target.transport_epoch) != self.epoch
            || self.generations.get(&target.thread_id) != Some(&target.subscription_generation)
        {
            return Err(Failure::Unqualified);
        }
        self.targets
            .iter()
            .find(|t| t.thread_id == target.thread_id)
            .map(|t| !t.busy)
            .ok_or(Failure::Unqualified)
    }
    fn publish(&mut self, epoch: Uuid, sequence: u64, targets: Vec<Target>) {
        self.generations
            .retain(|id, _| targets.iter().any(|t| t.thread_id == *id));
        for target in &targets {
            self.generations
                .entry(target.thread_id)
                .or_insert_with(Uuid::new_v4);
        }
        if let Some(attempt) = &mut self.attempt
            && self.generations.get(&attempt.target.thread_id)
                != Some(&attempt.target.subscription_generation)
        {
            attempt.fail(Failure::Unqualified)
        }
        if self.epoch != Some(epoch)
            || self
                .targets
                .iter()
                .map(|t| (t.thread_id, t.busy))
                .ne(targets.iter().map(|t| (t.thread_id, t.busy)))
        {
            self.revision = Uuid::new_v4()
        }
        self.epoch = Some(epoch);
        self.sequence = sequence;
        self.targets = targets;
    }
}
struct Shared {
    state: Mutex<State>,
    notify: Notify,
}
#[derive(Clone)]
pub(crate) struct Handle {
    run_id: Uuid,
    shared: Arc<Shared>,
    monitor: CaptureMonitor,
    control: ControlHandle,
}
pub(crate) struct Session {
    handle: Handle,
    task: JoinHandle<()>,
}
impl Session {
    pub(crate) fn start(run_id: Uuid, attachment: NativeAttachment) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::new()),
            notify: Notify::new(),
        });
        let handle = Handle {
            run_id,
            shared: shared.clone(),
            monitor: attachment.capture.subscribe_state(),
            control: attachment.control.clone(),
        };
        let task = tokio::spawn(observe(attachment.capture, attachment.control, shared));
        Self { handle, task }
    }
    pub(crate) fn handle(&self) -> Handle {
        self.handle.clone()
    }
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.handle.snapshot()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.handle
            .shared
            .state
            .lock()
            .unwrap()
            .fail("channel_run_stopped");
        self.handle.shared.notify.notify_waiters();
        self.task.abort()
    }
}
impl Handle {
    pub(crate) fn snapshot(&self) -> Snapshot {
        let mut state = self.shared.state.lock().unwrap();
        // Pin decoded state while reading source atomics, never the reverse.
        let _ = state.check_source(self.monitor.state(), self.control.is_closed());
        let snapshot = Snapshot {
            run_id: self.run_id,
            revision: state.revision,
            transport_epoch: state.epoch,
            phase: if state.failure.is_some() {
                "unavailable"
            } else if state.epoch.is_some() {
                "observing"
            } else {
                "waiting"
            },
            targets: state.targets.clone(),
            failure: state.failure,
        };
        self.shared.notify.notify_waiters();
        snapshot
    }
    pub(crate) fn qualified(&self, thread: Uuid) -> Result<QualifiedTarget, Failure> {
        self.qualified_revision(thread, None)
    }
    pub(crate) fn qualified_at(
        &self,
        thread: Uuid,
        revision: Uuid,
    ) -> Result<QualifiedTarget, Failure> {
        self.qualified_revision(thread, Some(revision))
    }
    fn qualified_revision(
        &self,
        thread: Uuid,
        revision: Option<Uuid>,
    ) -> Result<QualifiedTarget, Failure> {
        let mut state = self.shared.state.lock().unwrap();
        state.check_source(self.monitor.state(), self.control.is_closed())?;
        if revision.is_some_and(|revision| revision != state.revision) {
            return Err(Failure::SourceChanged);
        }
        let generation = *state.generations.get(&thread).ok_or(Failure::Unqualified)?;
        QualifiedTarget::new(
            self.run_id,
            thread,
            state.epoch.ok_or(Failure::Unqualified)?,
            generation,
        )
        .map_err(|_| Failure::Unqualified)
    }
    pub(crate) fn ready(&self, target: QualifiedTarget) -> Result<bool, Failure> {
        let mut state = self.shared.state.lock().unwrap();
        state.check_source(self.monitor.state(), self.control.is_closed())?;
        state.check_target(self.run_id, target)
    }
    pub(crate) fn arm(&self, target: QualifiedTarget, client: Uuid) -> Result<Attempt, Failure> {
        let mut state = self.shared.state.lock().unwrap();
        state.check_source(self.monitor.state(), self.control.is_closed())?;
        let id = state.arm(self.run_id, target, client)?;
        Ok(Attempt {
            handle: self.clone(),
            id,
            target,
            client,
        })
    }
}
/// Owns exactly one proof. Dropping this handle does not interrupt native work.
pub(crate) struct Attempt {
    handle: Handle,
    id: Uuid,
    target: QualifiedTarget,
    client: Uuid,
}
impl Attempt {
    pub(crate) async fn submit(&mut self, text: String) -> Submission {
        let allowed = {
            let mut state = self.handle.shared.state.lock().unwrap();
            let source =
                state.check_source(self.handle.monitor.state(), self.handle.control.is_closed());
            let target = state.check_target(self.handle.run_id, self.target);
            state
                .attempt
                .as_mut()
                .filter(|a| a.id == self.id)
                .is_some_and(|attempt| attempt.begin_submit(source.is_ok() && target == Ok(true)))
        };
        if !allowed {
            return Submission::NotSent {
                reason: ControlFailure::Protocol,
            };
        }
        // Marked before the first await; cancellation cannot allow a second send.
        let submission = self
            .handle
            .control
            .turn_start(self.target.thread_id, text, self.client)
            .await;
        {
            let mut state = self.handle.shared.state.lock().unwrap();
            let _ =
                state.check_source(self.handle.monitor.state(), self.handle.control.is_closed());
            if let Some(attempt) = state.attempt.as_mut().filter(|a| a.id == self.id) {
                attempt.acknowledge(submission)
            }
        }
        self.handle.shared.notify.notify_waiters();
        submission
    }
    pub(crate) fn revalidate(&self) -> Result<(), Failure> {
        let mut state = self.handle.shared.state.lock().unwrap();
        self.validate_locked(&mut state)
    }
    fn validate_locked(&self, state: &mut State) -> Result<(), Failure> {
        state.check_source(self.handle.monitor.state(), self.handle.control.is_closed())?;
        state.check_target(self.handle.run_id, self.target)?;
        let attempt = state
            .attempt
            .as_ref()
            .filter(|a| a.id == self.id)
            .ok_or(Failure::StaleAttempt)?;
        if let Some(failure) = attempt.failure {
            return Err(failure);
        }
        if attempt.proof.invalidated().is_some() {
            return Err(Failure::InvalidEvidence);
        }
        if !attempt.acknowledged {
            return Err(Failure::Submission);
        }
        Ok(())
    }
    pub(crate) async fn completion(&mut self) -> Result<Completion, Failure> {
        let mut monitor = self.handle.monitor.clone();
        loop {
            let notified = self.handle.shared.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.handle.shared.state.lock().unwrap();
                let caught_up = match state
                    .check_source(self.handle.monitor.state(), self.handle.control.is_closed())
                {
                    Ok(()) => true,
                    Err(Failure::Backlog) => false,
                    Err(e) => return Err(e),
                };
                state.check_target(self.handle.run_id, self.target)?;
                let attempt = state
                    .attempt
                    .as_mut()
                    .filter(|a| a.id == self.id)
                    .ok_or(Failure::StaleAttempt)?;
                if let Some(failure) = attempt.failure {
                    return Err(failure);
                }
                if caught_up && let Some(completion) = attempt.take_completion()? {
                    return Ok(completion);
                }
            }
            tokio::select! {_=notified=>{},_=monitor.changed()=>{},_=self.handle.control.closed()=>{}}
        }
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        let mut state = self.handle.shared.state.lock().unwrap();
        state.remove_attempt(self.id);
        self.handle.shared.notify.notify_waiters();
    }
}
fn invalid(capture: CaptureState, expected: Option<Uuid>) -> Option<&'static str> {
    if capture.gap_generation != 0 || capture.reason.is_some() {
        Some("channel_capture_gap")
    } else if expected.is_some() && capture.connection_epoch != expected {
        Some("channel_transport_changed")
    } else if capture.connection_epoch.is_some() && capture.closed {
        Some("channel_transport_closed")
    } else {
        None
    }
}
async fn observe(mut capture: CaptureReceiver, control: ControlHandle, shared: Arc<Shared>) {
    let mut monitor = capture.subscribe_state();
    let mut tracker = Tracker::new();
    let mut epoch = None;
    let mut sequence = 0_u64;
    loop {
        let frame = tokio::select! {
            _=control.closed()=>{shared.state.lock().unwrap().fail("channel_control_closed");shared.notify.notify_waiters();return;}
            current=monitor.changed()=>{if let Some(code)=invalid(current,epoch){shared.state.lock().unwrap().fail(code);shared.notify.notify_waiters();}continue;}
            frame=capture.recv()=>frame,
        };
        let Some(frame) = frame else {
            shared.state.lock().unwrap().fail("channel_capture_closed");
            shared.notify.notify_waiters();
            return;
        };
        let mut state = shared.state.lock().unwrap();
        if state.failure.is_some() {
            continue;
        }
        let current = capture.state();
        if let Some(code) = invalid(current, epoch) {
            state.fail(code);
            shared.notify.notify_waiters();
            continue;
        }
        if current.closed
            || current.connection_epoch != Some(frame.connection_epoch)
            || epoch.is_some_and(|id| id != frame.connection_epoch)
            || sequence.checked_add(1) != Some(frame.sequence)
        {
            state.fail("channel_capture_sequence");
            shared.notify.notify_waiters();
            continue;
        }
        epoch = Some(frame.connection_epoch);
        sequence = frame.sequence;
        let value = if frame.bytes.len() <= FRAME_LIMIT {
            serde_json::from_slice::<serde_json::Value>(&frame.bytes)
                .ok()
                .filter(|v| v.is_object())
        } else {
            None
        };
        let Some(value) = value else {
            state.fail("channel_protocol_invalid");
            shared.notify.notify_waiters();
            continue;
        };
        if tracker.observe(frame.direction, &value).is_err() {
            state.fail("channel_subscription_invalid");
            shared.notify.notify_waiters();
            continue;
        }
        if let Some(attempt) = &mut state.attempt {
            if frame.direction == Direction::ToServer {
                if evidence::local_input(&value, attempt.target.thread_id) {
                    attempt.fail(Failure::InvalidEvidence)
                }
            } else {
                match evidence::parse(&value, attempt.target.thread_id) {
                    Ok(Some(event)) => attempt.event(event),
                    Ok(None) => {}
                    Err(failure) => attempt.fail(failure),
                }
            }
        }
        if let Some(code) = invalid(capture.state(), epoch) {
            state.fail(code);
            shared.notify.notify_waiters();
            continue;
        }
        state.publish(
            frame.connection_epoch,
            sequence,
            tracker
                .targets()
                .into_iter()
                .map(|t| Target {
                    thread_id: t.thread_id,
                    busy: t.busy,
                })
                .collect(),
        );
        shared.notify.notify_waiters();
    }
}
#[cfg(test)]
mod tests;
