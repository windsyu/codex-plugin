use std::collections::{HashMap, HashSet};
use uuid::Uuid;

const MAX_ID: usize = 256;
const MAX_REPLY: usize = 64 * 1024;
const MAX_TERMINALS: usize = 32;
const MAX_FINALS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeStatus {
    Completed,
    Failed,
    Interrupted,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Withheld {
    MixedInput,
    IncompleteEvidence,
    InvalidOrder,
    IdentityConflict,
    CaptureGap,
    ReplyLimit,
    UnsuccessfulTurn,
}

/// Borrowed semantic events only: neither terminal bytes nor model response completion.
#[derive(Clone, Copy)]
pub enum Evidence<'a> {
    TurnStarted {
        thread: &'a str,
        turn: &'a str,
    },
    UserStarted {
        thread: &'a str,
        turn: &'a str,
        item: &'a str,
        client: Option<&'a str>,
    },
    UserCompleted {
        thread: &'a str,
        turn: &'a str,
        item: &'a str,
        client: Option<&'a str>,
    },
    FinalAnswer {
        thread: &'a str,
        turn: &'a str,
        item: &'a str,
        text: &'a str,
    },
    TurnCompleted {
        thread: &'a str,
        turn: &'a str,
        status: NativeStatus,
    },
    /// A turn identity returned by turn/start, when available, must agree with the echo.
    SubmissionTurn(&'a str),
    Gap,
}

/// No Debug: final answer text is private. Completion does not imply transport delivery.
pub struct Completion {
    pub status: NativeStatus,
    pub reply: Option<String>,
    pub withheld: Option<Withheld>,
}

struct Turn {
    id: String,
    user: Option<String>,
    user_completed: bool,
    channel: bool,
    taint: Option<Withheld>,
    final_answer: Option<(String, String)>,
    finals: HashMap<String, blake3::Hash>,
}

/// One submission attempt in one uninterrupted subscribed native connection.
/// Register before sending. A client ID proves origin, never idempotency. Dropping the
/// proof on disconnect/revocation is mandatory; reconnect cannot restore eligibility.
/// This filter excludes local-origin turns; a channel answer may quote its shared context.
pub struct OriginProof {
    thread: String,
    client: String,
    turn: Option<Turn>,
    acknowledged_turn: Option<String>,
    channel_turn: Option<String>,
    poison: Option<Withheld>,
    finished: bool,
    terminals: HashSet<String>,
}

impl OriginProof {
    pub fn new(thread: &str, client: Uuid) -> Option<Self> {
        valid_id(thread).then(|| Self {
            thread: thread.to_owned(),
            client: client.to_string(),
            turn: None,
            acknowledged_turn: None,
            channel_turn: None,
            poison: None,
            finished: false,
            terminals: HashSet::new(),
        })
    }

    pub fn observe(&mut self, event: Evidence<'_>) -> Option<Completion> {
        match event {
            Evidence::Gap => self.poison(Withheld::CaptureGap),
            Evidence::SubmissionTurn(turn) => {
                if !valid_id(turn)
                    || self
                        .acknowledged_turn
                        .as_deref()
                        .is_some_and(|id| id != turn)
                    || self.channel_turn.as_deref().is_some_and(|id| id != turn)
                {
                    self.poison(Withheld::IdentityConflict);
                } else {
                    self.acknowledged_turn = Some(turn.to_owned());
                }
            }
            Evidence::TurnStarted { thread, turn } if thread == self.thread => {
                if self.finished || !valid_id(turn) || self.terminals.contains(turn) {
                    self.poison(Withheld::InvalidOrder);
                } else if let Some(active) = &self.turn {
                    if active.id != turn {
                        self.poison(Withheld::InvalidOrder);
                    }
                } else {
                    self.turn = Some(Turn {
                        id: turn.to_owned(),
                        user: None,
                        user_completed: false,
                        channel: false,
                        taint: None,
                        final_answer: None,
                        finals: HashMap::new(),
                    });
                }
            }
            Evidence::UserStarted {
                thread,
                turn,
                item,
                client,
            }
            | Evidence::UserCompleted {
                thread,
                turn,
                item,
                client,
            } if thread == self.thread => {
                let completed = matches!(event, Evidence::UserCompleted { .. });
                self.user(turn, item, client, completed);
            }
            Evidence::FinalAnswer {
                thread,
                turn,
                item,
                text,
            } if thread == self.thread => {
                if self.finished {
                    self.poison(Withheld::InvalidOrder);
                } else if let Some(active) = self.turn.as_mut().filter(|active| active.id == turn) {
                    if !active.channel || !active.user_completed {
                        active.taint.get_or_insert(Withheld::IncompleteEvidence);
                    } else if !valid_id(item) || text.len() > MAX_REPLY {
                        active.taint.get_or_insert(Withheld::ReplyLimit);
                    } else if let Some(prior) = active.finals.get(item) {
                        if *prior != blake3::hash(text.as_bytes()) {
                            active.taint.get_or_insert(Withheld::IdentityConflict);
                        }
                    } else if active.finals.len() >= MAX_FINALS {
                        active.taint.get_or_insert(Withheld::ReplyLimit);
                    } else {
                        // Only an explicitly final native assistant item enters this API.
                        active
                            .finals
                            .insert(item.to_owned(), blake3::hash(text.as_bytes()));
                        active.final_answer = Some((item.to_owned(), text.to_owned()));
                    }
                } else {
                    self.poison(Withheld::IncompleteEvidence);
                }
            }
            Evidence::TurnCompleted {
                thread,
                turn,
                status,
            } if thread == self.thread => {
                if self.finished {
                    return None; // terminal duplicate never reopens or resends the attempt
                }
                let Some(active) = self.turn.take() else {
                    self.terminal(turn);
                    if self.channel_turn.as_deref() == Some(turn) {
                        self.poison(Withheld::InvalidOrder);
                    }
                    return None; // an already active local turn may predate registration
                };
                if active.id != turn {
                    self.turn = Some(active);
                    self.poison(Withheld::InvalidOrder);
                    return None;
                }
                self.terminal(turn);
                if !active.channel {
                    return None; // serial queue may wait behind local work
                }
                self.finished = true;
                let withheld = self.poison.or(active.taint).or_else(|| {
                    if !active.user_completed {
                        Some(Withheld::IncompleteEvidence)
                    } else if status != NativeStatus::Completed {
                        Some(Withheld::UnsuccessfulTurn)
                    } else {
                        None
                    }
                });
                return Some(Completion {
                    status,
                    reply: withheld
                        .is_none()
                        .then(|| active.final_answer.map(|(_, text)| text))
                        .flatten(),
                    withheld,
                });
            }
            _ => {}
        }
        None
    }

    /// Callers also consult this after late events; previously delivered content cannot be recalled.
    pub fn invalidated(&self) -> Option<Withheld> {
        self.poison
    }

    fn poison(&mut self, reason: Withheld) {
        self.poison.get_or_insert(reason);
    }

    fn terminal(&mut self, turn: &str) {
        if !valid_id(turn)
            || (self.terminals.len() >= MAX_TERMINALS && !self.terminals.contains(turn))
        {
            self.poison(Withheld::CaptureGap);
        } else {
            self.terminals.insert(turn.to_owned());
        }
    }

    fn user(&mut self, turn: &str, item: &str, client: Option<&str>, completed: bool) {
        if self.finished || !valid_id(turn) || !valid_id(item) {
            self.poison(Withheld::InvalidOrder);
            return;
        }
        let channel = client == Some(self.client.as_str());
        if channel {
            if self.channel_turn.as_deref().is_some_and(|id| id != turn)
                || self
                    .acknowledged_turn
                    .as_deref()
                    .is_some_and(|id| id != turn)
            {
                self.poison(Withheld::IdentityConflict);
            }
            self.channel_turn = Some(turn.to_owned());
        }
        let Some(active) = self.turn.as_mut().filter(|active| active.id == turn) else {
            self.poison(Withheld::IncompleteEvidence);
            return;
        };
        active.channel |= channel;
        if !channel {
            active.taint.get_or_insert(Withheld::MixedInput);
        }
        if let Some(first) = &active.user {
            if first != item {
                active.taint.get_or_insert(Withheld::MixedInput);
            }
        } else if completed {
            active.taint.get_or_insert(Withheld::IncompleteEvidence);
        } else {
            active.user = Some(item.to_owned());
        }
        if completed && active.user.as_deref() == Some(item) {
            active.user_completed = true;
        }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID && !id.chars().any(char::is_control)
}

#[cfg(test)]
mod tests;
