//! In-memory, locally confirmed authorization for exactly one private chat.
//! A restart creates a new generation; credentials and message bodies never live here.
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::telegram::{self, Inbound, TelegramBinding, client::BotIdentity};

const PAIR_LIFETIME: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QualifiedTarget {
    pub run_epoch: Uuid,
    pub thread_id: Uuid,
    pub transport_epoch: Uuid,
    pub subscription_generation: Uuid,
}
impl QualifiedTarget {
    /// The caller must first verify this exact target against the current TUI
    /// subscription and loaded-thread evidence. This only validates identifier
    /// shape; it never discovers, loads, resumes, or guesses a thread.
    pub(crate) fn new(
        run_epoch: Uuid,
        thread_id: Uuid,
        transport_epoch: Uuid,
        subscription_generation: Uuid,
    ) -> Result<Self, Failure> {
        let target = Self {
            run_epoch,
            thread_id,
            transport_epoch,
            subscription_generation,
        };
        if !target.valid() {
            return Err(Failure::InvalidIdentity);
        }
        Ok(target)
    }
    fn valid(self) -> bool {
        !self.run_epoch.is_nil()
            && !self.thread_id.is_nil()
            && !self.transport_epoch.is_nil()
            && !self.subscription_generation.is_nil()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Candidate {
    pub generation: Uuid,
    pub bot_id: i64,
    pub user_id: i64,
    pub chat_id: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActiveBinding {
    pub generation: Uuid,
    pub bot_id: i64,
    pub user_id: i64,
    pub chat_id: i64,
    pub target: QualifiedTarget,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Phase {
    Unconfigured,
    Idle,
    Pairing,
    AwaitingConfirmation,
    Active,
    Expired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub phase: Phase,
    pub generation: Uuid,
    pub bot_id: Option<i64>,
    pub candidate: Option<Candidate>,
    pub active: Option<ActiveBinding>,
}
// Deliberately not Debug/Serialize: only the local begin-pair response gets nonce.
pub(crate) struct PairingCode {
    pub generation: Uuid,
    pub nonce: String,
    pub expires_in: Duration,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    InvalidIdentity,
    InvalidMessage,
    WrongBot,
    NotPairing,
    InvalidNonce,
    Expired,
    CandidateMismatch,
    Inactive,
    StaleGeneration,
    WrongTarget,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidIdentity => "channel binding invalid identity",
            Self::InvalidMessage => "channel binding invalid message",
            Self::WrongBot => "channel binding wrong bot",
            Self::NotPairing => "channel binding not pairing",
            Self::InvalidNonce => "channel binding invalid nonce",
            Self::Expired => "channel binding pairing expired",
            Self::CandidateMismatch => "channel binding candidate mismatch",
            Self::Inactive => "channel binding inactive",
            Self::StaleGeneration => "channel binding stale generation",
            Self::WrongTarget => "channel binding wrong target",
        })
    }
}
impl std::error::Error for Failure {}

// Neither private pairing nonce nor a payload can appear in Debug output.
enum State {
    Idle,
    Pairing {
        nonce: String,
        started: Instant,
    },
    Candidate {
        candidate: Candidate,
        started: Instant,
    },
    Active(ActiveBinding),
    Expired,
}
pub(crate) struct Binding {
    generation: Uuid,
    bot: Option<BotIdentity>,
    state: State,
}
impl Default for Binding {
    fn default() -> Self {
        Self::new()
    }
}
impl Binding {
    pub(crate) fn new() -> Self {
        Self {
            generation: Uuid::new_v4(),
            bot: None,
            state: State::Idle,
        }
    }
    pub(crate) fn replace_bot(&mut self, bot: BotIdentity) -> Result<(), Failure> {
        if bot.id <= 0 {
            return Err(Failure::InvalidIdentity);
        }
        self.bot = Some(bot);
        self.reset(State::Idle);
        Ok(())
    }
    pub(crate) fn begin_pair(
        &mut self,
        bot: BotIdentity,
        now: Instant,
    ) -> Result<PairingCode, Failure> {
        self.replace_bot(bot)?;
        let nonce = Uuid::new_v4().to_string();
        self.state = State::Pairing {
            nonce: nonce.clone(),
            started: now,
        };
        Ok(PairingCode {
            generation: self.generation,
            nonce,
            expires_in: PAIR_LIFETIME,
        })
    }
    /// Only a direct, original private text command can nominate a candidate.
    /// Knowing the nonce does not activate input or grant access to any content.
    pub(crate) fn pair(
        &mut self,
        bot_id: i64,
        update: &Value,
        now: Instant,
    ) -> Result<Candidate, Failure> {
        self.expire(now);
        self.check_bot(bot_id)?;
        let (expected_nonce, started) = match &self.state {
            State::Pairing { nonce, started } => (nonce, *started),
            State::Expired => return Err(Failure::Expired),
            _ => return Err(Failure::NotPairing),
        };
        let message = update
            .get("message")
            .and_then(Value::as_object)
            .ok_or(Failure::InvalidMessage)?;
        // Pairing is narrower than ordinary chat text. In particular, an edited,
        // forwarded, or quoted command must not borrow another sender's nonce.
        if message.keys().any(|key| {
            !matches!(
                key.as_str(),
                "message_id" | "from" | "chat" | "date" | "text" | "entities"
            )
        }) {
            return Err(Failure::InvalidMessage);
        }
        let positive = |value: &Value| {
            value
                .as_i64()
                .filter(|id| *id > 0)
                .ok_or(Failure::InvalidIdentity)
        };
        let user_id = positive(&update["message"]["from"]["id"])?;
        let chat_id = positive(&update["message"]["chat"]["id"])?;
        let temporary = TelegramBinding {
            bot_id,
            user_id,
            chat_id,
            generation: self.generation,
        };
        let Inbound::Text { text, .. } = telegram::authorize(update, &temporary) else {
            return Err(Failure::InvalidMessage);
        };
        // Telegram normally includes exactly this entity for a slash command.
        // A link, mention, overlapping entity, or a different command extent is
        // ambiguous here and is rejected instead of being interpreted as /bind.
        if let Some(entities) = message.get("entities") {
            let entities = entities.as_array().ok_or(Failure::InvalidMessage)?;
            if entities.len() != 1 {
                return Err(Failure::InvalidMessage);
            }
            let entity = entities[0].as_object().ok_or(Failure::InvalidMessage)?;
            if entity.len() != 3
                || entity.get("type").and_then(Value::as_str) != Some("bot_command")
                || entity.get("offset").and_then(Value::as_u64) != Some(0)
                || entity.get("length").and_then(Value::as_u64) != Some(5)
            {
                return Err(Failure::InvalidMessage);
            }
        }
        let supplied_nonce = text.strip_prefix("/bind ").ok_or(Failure::InvalidMessage)?;
        if supplied_nonce != expected_nonce {
            return Err(Failure::InvalidNonce);
        }
        let candidate = Candidate {
            generation: self.generation,
            bot_id,
            user_id,
            chat_id,
        };
        // Consuming nonce removes it entirely. Confirmation keeps the original
        // deadline; submitting a candidate never renews the ten-minute lease.
        self.state = State::Candidate { candidate, started };
        Ok(candidate)
    }
    pub(crate) fn confirm(
        &mut self,
        candidate: Candidate,
        target: QualifiedTarget,
        now: Instant,
    ) -> Result<ActiveBinding, Failure> {
        self.expire(now);
        if !target.valid() {
            return Err(Failure::InvalidIdentity);
        }
        match &self.state {
            State::Candidate {
                candidate: expected,
                ..
            } if *expected == candidate => {}
            State::Expired => return Err(Failure::Expired),
            _ => return Err(Failure::CandidateMismatch),
        }
        self.check_bot(candidate.bot_id)?;
        self.reset(State::Idle);
        let active = ActiveBinding {
            generation: self.generation,
            bot_id: candidate.bot_id,
            user_id: candidate.user_id,
            chat_id: candidate.chat_id,
            target,
        };
        self.state = State::Active(active);
        Ok(active)
    }
    pub(crate) fn authorize(
        &self,
        bot_id: i64,
        generation: Uuid,
        update: &Value,
    ) -> Result<Inbound, Failure> {
        self.check_bot(bot_id)?;
        let active = self.current(generation)?;
        Ok(telegram::authorize(
            update,
            &TelegramBinding {
                bot_id: active.bot_id,
                user_id: active.user_id,
                chat_id: active.chat_id,
                generation,
            },
        ))
    }
    /// Repeat immediately before native submission and before each external send.
    pub(crate) fn revalidate(
        &self,
        generation: Uuid,
        target: QualifiedTarget,
    ) -> Result<ActiveBinding, Failure> {
        let active = self.current(generation)?;
        if !target.valid() || active.target != target {
            return Err(Failure::WrongTarget);
        }
        Ok(active)
    }
    /// Target-specific delayed events cannot revoke a newer target/generation.
    /// A caller handling a whole-transport gap supplies None with the generation
    /// captured by that consumer, not the current generation looked up later.
    pub(crate) fn invalidate(
        &mut self,
        expected_generation: Uuid,
        expected_target: Option<QualifiedTarget>,
    ) -> bool {
        if expected_generation.is_nil() || expected_generation != self.generation {
            return false;
        }
        if let Some(target) = expected_target
            && !matches!(self.state, State::Active(active) if active.target == target)
        {
            return false;
        }
        self.revoke();
        true
    }
    pub(crate) fn revoke(&mut self) {
        self.reset(State::Idle);
    }
    pub(crate) fn snapshot(&mut self, now: Instant) -> Snapshot {
        self.expire(now);
        let (phase, candidate, active) = match self.state {
            State::Idle => (
                if self.bot.is_some() {
                    Phase::Idle
                } else {
                    Phase::Unconfigured
                },
                None,
                None,
            ),
            State::Pairing { .. } => (Phase::Pairing, None, None),
            State::Candidate { candidate, .. } => {
                (Phase::AwaitingConfirmation, Some(candidate), None)
            }
            State::Active(active) => (Phase::Active, None, Some(active)),
            State::Expired => (Phase::Expired, None, None),
        };
        Snapshot {
            phase,
            generation: self.generation,
            bot_id: self.bot.as_ref().map(|bot| bot.id),
            candidate,
            active,
        }
    }
    fn check_bot(&self, bot_id: i64) -> Result<(), Failure> {
        if bot_id <= 0 || self.bot.as_ref().map(|bot| bot.id) != Some(bot_id) {
            return Err(Failure::WrongBot);
        }
        Ok(())
    }
    fn current(&self, generation: Uuid) -> Result<ActiveBinding, Failure> {
        if generation.is_nil() || generation != self.generation {
            return Err(Failure::StaleGeneration);
        }
        match self.state {
            State::Active(active) => Ok(active),
            _ => Err(Failure::Inactive),
        }
    }
    fn expire(&mut self, now: Instant) {
        let started = match self.state {
            State::Pairing { started, .. } | State::Candidate { started, .. } => started,
            _ => return,
        };
        if now
            .checked_duration_since(started)
            .is_none_or(|elapsed| elapsed >= PAIR_LIFETIME)
        {
            self.reset(State::Expired);
        }
    }
    fn reset(&mut self, state: State) {
        let mut next = Uuid::new_v4();
        while next == self.generation {
            next = Uuid::new_v4();
        }
        self.generation = next;
        self.state = state;
    }
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;
