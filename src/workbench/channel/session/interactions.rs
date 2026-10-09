//! Ephemeral native requests. IDs and scopes are never evicted for reuse.
use super::{Failure, Handle, PendingAttempt, QualifiedTarget, State};
use crate::workbench::{
    channel::{
        interaction::{self, Action, Answer, Decode, FileScope, NativeResponse, RpcId},
        native::Direction,
    },
    launch::{ControlFailure, ResponseWrite},
};
use serde_json::Value;
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};
use uuid::Uuid;

const LIMIT: usize = 32;
const DEADLINE: Duration = Duration::from_secs(10 * 60);

pub(crate) struct Prompt {
    pub id: Uuid,
    pub request: interaction::Request,
}
struct Entry {
    id: Uuid,
    request: interaction::Request,
    offered: bool,
    retired: bool,
    deadline: Instant,
}
struct QuestionPending {
    id: RpcId,
    turn: String,
    item: String,
    retired: bool,
}
struct Scope {
    full: FileScope,
    retired: bool,
}
#[derive(Default)]
pub(super) struct Pending {
    used: HashSet<RpcId>,
    entries: Vec<Entry>,
    scopes: Vec<Scope>,
    questions: Vec<QuestionPending>,
    failed: bool,
    notice_turns: HashSet<String>,
    notice_taken: bool,
}
impl Pending {
    pub(super) fn invalidate(&mut self) {
        for entry in &mut self.entries {
            entry.retired = true;
        }
        for scope in &mut self.scopes {
            scope.retired = true;
        }
        for question in &mut self.questions {
            question.retired = true;
        }
    }
    fn suppress(&mut self) {
        self.failed = true;
        self.invalidate();
    }
    fn notice(&mut self, value: &Value) {
        if self.notice_turns.len() < LIMIT
            && let Some(turn) = value["params"]["turnId"].as_str()
            && !turn.is_empty()
            && turn.len() <= 128
            && !turn.chars().any(char::is_control)
        {
            self.notice_turns.insert(turn.to_owned());
        }
    }
    pub(super) fn observe(&mut self, direction: Direction, value: &Value, thread: Uuid) {
        // Native TUI responses retire immediately, with typed RPC identity.
        if direction == Direction::ToServer {
            if value.get("method").is_none()
                && (value.get("result").is_some() || value.get("error").is_some())
                && let Ok(id) = RpcId::parse(&value["id"])
            {
                for entry in &mut self.entries {
                    if entry.request.id == id {
                        entry.retired = true;
                    }
                }
                for question in &mut self.questions {
                    if question.id == id {
                        question.retired = true;
                    }
                }
            }
            return;
        }
        let own = value["params"]["threadId"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            == Some(thread);
        let method = value["method"].as_str();
        if value.get("id").is_some() && method.is_some() {
            // Include unsupported and foreign requests in the same bounded ID
            // namespace; a reused native ID makes a prior answer ambiguous.
            let Ok(id) = RpcId::parse(&value["id"]) else {
                if own {
                    self.notice(value);
                }
                self.suppress();
                return;
            };
            if self.used.len() >= LIMIT || !self.used.insert(id.clone()) {
                if own {
                    self.notice(value);
                }
                self.suppress();
                return;
            }
            if !own {
                return;
            }
            if method == Some("item/tool/requestUserInput") {
                let identity = |key: &str| {
                    value["params"][key].as_str().filter(|s| {
                        !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
                    })
                };
                if let (Some(turn), Some(item)) = (identity("turnId"), identity("itemId")) {
                    self.questions.push(QuestionPending {
                        id,
                        turn: turn.to_owned(),
                        item: item.to_owned(),
                        retired: false,
                    });
                } else {
                    self.notice(value);
                    self.suppress();
                    return;
                }
            }
            if self.failed {
                self.notice(value);
                return;
            }
            let scope = self.scopes.iter().find(|scope| {
                !scope.retired
                    && scope.full.thread == thread
                    && Some(scope.full.turn.as_str()) == value["params"]["turnId"].as_str()
                    && Some(scope.full.item.as_str()) == value["params"]["itemId"].as_str()
            });
            match interaction::decode_request(value, scope.map(|scope| &scope.full)) {
                Ok(Decode::Supported(request)) => self.entries.push(Entry {
                    id: Uuid::new_v4(),
                    request,
                    offered: false,
                    retired: false,
                    deadline: Instant::now() + DEADLINE,
                }),
                _ => self.notice(value),
            }
            return;
        }
        if !own || value.get("id").is_some() {
            return;
        }
        match method {
            Some("serverRequest/resolved") => {
                if let Ok(id) = RpcId::parse(&value["params"]["requestId"]) {
                    for entry in &mut self.entries {
                        if entry.request.id == id {
                            entry.retired = true;
                        }
                    }
                    for question in &mut self.questions {
                        if question.id == id {
                            question.retired = true;
                        }
                    }
                }
            }
            Some("item/completed") => {
                let turn = value["params"]["turnId"].as_str();
                let item = value["params"]["item"]["id"].as_str();
                for entry in &mut self.entries {
                    if Some(entry.request.turn.as_str()) == turn
                        && Some(entry.request.item.as_str()) == item
                    {
                        entry.retired = true;
                    }
                }
                for question in &mut self.questions {
                    if Some(question.turn.as_str()) == turn && Some(question.item.as_str()) == item
                    {
                        question.retired = true;
                    }
                }
                for scope in &mut self.scopes {
                    if Some(scope.full.turn.as_str()) == turn
                        && Some(scope.full.item.as_str()) == item
                    {
                        scope.retired = true;
                    }
                }
            }
            Some("item/started") if value["params"]["item"]["type"] == "fileChange" => {
                if self.failed {
                    return;
                }
                match FileScope::from_started(value) {
                    Ok(Some(full)) => {
                        if let Some(existing) = self.scopes.iter().find(|scope| {
                            scope.full.thread == full.thread
                                && scope.full.turn == full.turn
                                && scope.full.item == full.item
                        }) {
                            if existing.retired || existing.full != full {
                                self.notice(value);
                                self.suppress();
                            }
                        } else if self.scopes.len() >= LIMIT {
                            self.notice(value);
                            self.suppress();
                        } else {
                            self.scopes.push(Scope {
                                full,
                                retired: false,
                            });
                        }
                    }
                    _ => {
                        self.notice(value);
                        self.suppress();
                    }
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone)]
pub(crate) struct InteractionHandle {
    handle: Handle,
    attempt: Uuid,
    target: QualifiedTarget,
}
impl InteractionHandle {
    pub(super) fn new(handle: Handle, attempt: Uuid, target: QualifiedTarget) -> Self {
        Self {
            handle,
            attempt,
            target,
        }
    }
    fn validate<'a>(&self, state: &'a mut State) -> Result<&'a mut PendingAttempt, Failure> {
        // Pin decoder state first, then read source atomics. Never trust a cached watermark.
        state.check_source(self.handle.monitor.state(), self.handle.control.is_closed())?;
        state.check_target(self.handle.run_id, self.target)?;
        let attempt = state
            .attempt
            .as_mut()
            .filter(|a| a.id == self.attempt)
            .ok_or(Failure::StaleAttempt)?;
        attempt.interaction_live()?;
        Ok(attempt)
    }
    pub(crate) fn next(&self) -> Result<Option<Prompt>, Failure> {
        let mut state = self.handle.shared.state.lock().unwrap();
        let attempt = self.validate(&mut state)?;
        attempt.next_interaction()
    }

    pub(crate) fn current(&self, id: Uuid) -> Result<(), Failure> {
        let mut state = self.handle.shared.state.lock().unwrap();
        self.validate(&mut state)?.interaction_entry(id).map(|_| ())
    }
    #[cfg(test)]
    pub(crate) fn expire_for_test(&self, id: Uuid) {
        let mut state = self.handle.shared.state.lock().unwrap();
        if let Some(attempt) = state
            .attempt
            .as_mut()
            .filter(|attempt| attempt.id == self.attempt)
            && let Some(entry) = attempt
                .interactions
                .entries
                .iter_mut()
                .find(|entry| entry.id == id)
        {
            entry.deadline = Instant::now() - Duration::from_secs(1);
        }
    }
    pub(crate) fn awaiting_question(&self) -> Result<bool, Failure> {
        let mut state = self.handle.shared.state.lock().unwrap();
        self.validate(&mut state)?.awaiting_question()
    }
    pub(crate) fn take_local_notice(&self) -> Result<bool, Failure> {
        let mut state = self.handle.shared.state.lock().unwrap();
        let attempt = self.validate(&mut state)?;
        let Some(turn) = attempt.owned_turn() else {
            return Ok(false);
        };
        if attempt.interactions.notice_taken || !attempt.interactions.notice_turns.contains(turn) {
            return Ok(false);
        }
        attempt.interactions.notice_taken = true;
        Ok(true)
    }
    pub(crate) async fn answer(
        &self,
        id: Uuid,
        action: Action,
        selected: Vec<Answer>,
        validate: Box<dyn FnOnce() -> bool + Send>,
    ) -> ResponseWrite {
        let prepared = {
            let mut state = self.handle.shared.state.lock().unwrap();
            self.validate(&mut state).and_then(|attempt| {
                let entry = attempt.interaction_entry(id)?;
                let response = entry
                    .request
                    .response(&action, &selected)
                    .ok()
                    .flatten()
                    .ok_or(Failure::InteractionUnavailable)?;
                Ok((entry.request.id.clone(), response))
            })
        };
        self.respond_prepared(id, prepared, validate).await
    }
    pub(crate) async fn answer_text(
        &self,
        id: Uuid,
        question: usize,
        text: String,
        mut selected: Vec<Answer>,
        validate: Box<dyn FnOnce() -> bool + Send>,
    ) -> ResponseWrite {
        let prepared = {
            let mut state = self.handle.shared.state.lock().unwrap();
            self.validate(&mut state).and_then(|attempt| {
                let entry = attempt.interaction_entry(id)?;
                let answer = entry
                    .request
                    .answer_text(question, &text)
                    .map_err(|_| Failure::InteractionUnavailable)?;
                selected.push(answer);
                let response = entry
                    .request
                    .answers_response(&selected)
                    .ok()
                    .flatten()
                    .ok_or(Failure::InteractionUnavailable)?;
                Ok((entry.request.id.clone(), response))
            })
        };
        self.respond_prepared(id, prepared, validate).await
    }
    async fn respond_prepared(
        &self,
        id: Uuid,
        prepared: Result<(RpcId, NativeResponse), Failure>,
        validate: Box<dyn FnOnce() -> bool + Send>,
    ) -> ResponseWrite {
        let Ok((rpc_id, response)) = prepared else {
            return ResponseWrite::NotSent {
                reason: ControlFailure::InvalidInput,
            };
        };
        let current = self.clone();
        self.handle
            .control
            .respond(
                rpc_id,
                response,
                Box::new(move || {
                    // Global ordering is Core -> Session. The external authorization
                    // closure must finish first; never invoke it under the session lock.
                    if !validate() {
                        return false;
                    }
                    let mut state = current.handle.shared.state.lock().unwrap();
                    let Ok(attempt) = current.validate(&mut state) else {
                        return false;
                    };
                    let Ok(entry) = attempt.interaction_entry(id) else {
                        return false;
                    };
                    entry.retired = true;
                    // Consuming remote authority does not prove native adoption.
                    // Keep question admission blocked until native retirement evidence.
                    true
                }),
            )
            .await
    }
}
impl PendingAttempt {
    fn awaiting_question(&self) -> Result<bool, Failure> {
        self.interaction_live()?;
        if self.interactions.failed {
            return Err(Failure::InteractionUnavailable);
        }
        // Missing ACK or complete origin evidence is unknown classification,
        // never evidence that native is ready for another queued text task.
        let turn = self.owned_turn().ok_or(Failure::InteractionUnavailable)?;
        // Only native retirement evidence ends a pending question. Remote
        // button expiry cannot turn free text into a new task while native waits.
        Ok(self
            .interactions
            .questions
            .iter()
            .any(|question| !question.retired && question.turn == turn))
    }
    fn next_interaction(&mut self) -> Result<Option<Prompt>, Failure> {
        self.interaction_live()?;
        if self.interactions.failed {
            return Err(Failure::InteractionUnavailable);
        }
        let Some(turn) = self.owned_turn() else {
            return Ok(None);
        };
        let turn = turn.to_owned();
        let now = Instant::now();
        let entry = self.interactions.entries.iter_mut().find(|entry| {
            !entry.offered && !entry.retired && entry.deadline > now && entry.request.turn == turn
        });
        Ok(entry.map(|entry| {
            entry.offered = true;
            Prompt {
                id: entry.id,
                request: entry.request.clone(),
            }
        }))
    }
    fn interaction_live(&self) -> Result<(), Failure> {
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        if self.taken || self.terminal.is_some() || self.proof.invalidated().is_some() {
            return Err(Failure::StaleAttempt);
        }
        Ok(())
    }
    fn owned_turn(&self) -> Option<&str> {
        self.acknowledged
            .then(|| self.proof.live_owned_turn())
            .flatten()
    }
    fn interaction_entry(&mut self, id: Uuid) -> Result<&mut Entry, Failure> {
        self.interaction_live()?;
        if self.interactions.failed {
            return Err(Failure::InteractionUnavailable);
        }
        let turn = self
            .owned_turn()
            .ok_or(Failure::InteractionUnavailable)?
            .to_owned();
        self.interactions
            .entries
            .iter_mut()
            .find(|entry| {
                entry.id == id
                    && entry.offered
                    && !entry.retired
                    && entry.deadline > Instant::now()
                    && entry.request.thread == self.target.thread_id
                    && entry.request.turn == turn
            })
            .ok_or(Failure::InteractionUnavailable)
    }
}

#[cfg(test)]
mod tests;
