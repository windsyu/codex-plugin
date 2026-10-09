//! Ingress identities only; reservations grant no interaction authority.
use std::collections::BTreeMap;

use uuid::Uuid;

use super::{DeliveryKey, private_message_key};

const LIMIT: usize = 17;

// Never retain or format the callback query, answer body, or reference here.
#[derive(Clone, Copy)]
pub(super) struct Identity {
    pub bot: i64,
    pub update: i64,
    pub key: Option<DeliveryKey>,
    pub query: Option<[u8; 32]>,
}
impl Identity {
    pub(super) fn new(bot: i64, update: i64, value: &serde_json::Value) -> Self {
        let query = value["callback_query"]["id"]
            .as_str()
            .filter(|query| crate::workbench::channel::store::valid_query_id(query))
            .map(|query| *blake3::hash(query.as_bytes()).as_bytes());
        Self {
            bot,
            update,
            key: private_message_key(bot, value),
            query,
        }
    }
    fn conflicts(&self, other: &Self) -> bool {
        (self.bot == other.bot
            && (self.update == other.update
                || self.query.is_some_and(|query| Some(query) == other.query)))
            || self.key.is_some_and(|key| Some(key) == other.key)
    }
}
#[derive(Default)]
pub(super) struct Pending(BTreeMap<Uuid, Identity>);
pub(super) enum Reservation {
    Owned(Uuid),
    Conflict,
    Full,
}
impl Pending {
    pub(super) fn reserve(&mut self, identity: Identity) -> Reservation {
        if self.0.values().any(|pending| pending.conflicts(&identity)) {
            return Reservation::Conflict;
        }
        if self.0.len() >= LIMIT {
            return Reservation::Full;
        }
        let owner = Uuid::new_v4();
        self.0.insert(owner, identity);
        Reservation::Owned(owner)
    }
    pub(super) fn release(&mut self, owner: Uuid) {
        self.0.remove(&owner);
    }
}
